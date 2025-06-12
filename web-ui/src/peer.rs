use crate::{
    display_bytes,
    file::{File, FileDisplayContext},
    FilesSignal, PeerPath,
};
use leptos::{either::Either, prelude::*};
use std::collections::{HashMap, HashSet};
use std::ops::Bound::Included;
use thaw::*;

// #[derive(Clone, Debug)]
// pub struct Peer {
//     pub name: String,
//     pub is_self: bool,
// }
//
// impl Peer {
//     pub fn new(name: String, is_self: bool) -> Self {
//         Self { name, is_self }
//     }
// }

#[component]
pub fn Peer(name: String, is_self: bool) -> impl IntoView {
    let files = use_context::<FilesSignal>().unwrap().0;
    // This signal is used below to provide context to File
    let (peer_signal, _set_peer) = signal((name.clone(), is_self));

    // This should probably be in a closure
    let root_size = display_bytes(
        match files.get().get(&PeerPath {
            peer_name: name,
            path: "".to_string(),
        }) {
            Some(file) => file.size.unwrap_or_default(),
            None => 0,
        },
    );

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
            .filter(|(_, file)| file.is_visible.get())
            .map(|(_, file)| file.clone()) // TODO ideally dont clone
            .collect::<Vec<File>>()
    };

    view! {
        <div>
            <Flex vertical=true>
                <div>
                    <Icon icon=icondata::AiUserOutlined />
                    {peer_signal.get().0}
                    " "
                    {root_size}
                    " shared"
                </div>
                <Table>
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
            </Flex>
        </div>
    }
}

#[component]
pub fn Peers(
    peers: ReadSignal<HashSet<String>>,
    announce_address: ReadSignal<Option<String>>,
    pending_peers: ReadSignal<HashSet<String>>,
    set_pending_peers: WriteSignal<HashSet<String>>,
) -> impl IntoView {
    let show_peers = move || {
        if peers.get().is_empty() {
            Either::Left(view! {
                <div>
                    <p>"No peers connected"</p>
                </div>
            })
        } else {
            Either::Right(view! {
                <div>
                    <For
                        each=move || peers.get()
                        key=|name| name.clone()
                        children=move |name| view! { <Peer name is_self=false /> }
                    />
                </div>
            })
        }
    };

    let show_pending_peers = move || {
        view! {
            <For
                each=move || pending_peers.get()
                key=|announce_address| announce_address.clone()
                children=move |announce_address| {
                    view! {
                        <Flex>
                            <Spinner label=announce_address size=SpinnerSize::Small />
                        </Flex>
                    }
                }
            />
        }
    };

    // let set_requester = use_context::<RequesterSetter>().unwrap().0;
    let input_value = RwSignal::new(String::new());

    let add_peer = move |_| {
        let announce_payload = input_value.get();
        let announce_payload = announce_payload.trim();
        if !announce_payload.is_empty() {
            // let add = Command::ConnectDirect(announce_payload.to_string());
            // set_requester.update(|requester| requester.make_request(add));
            set_pending_peers.update(|pending_peers| {
                pending_peers.insert(announce_payload.to_string());
            });
        }

        input_value.set(String::new());
    };

    let announce = move || {
        announce_address
            .get()
            .unwrap_or("No announce address".to_string())
    };

    let copy_to_clipboard = move |_| {
        wasm_bindgen_futures::spawn_local(async move {
            let window = web_sys::window().unwrap();
            let clipboard = window.navigator().clipboard();
            let promise = clipboard.write_text(
                &announce_address
                    .get_untracked()
                    .unwrap_or("Cannot get signal".to_string()),
            );
            let _result = wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
            log::info!("Copied to clipboard");
        });
    };

    view! {
        <p>
            Announce address<code>{announce}</code> <Popover trigger_type=PopoverTriggerType::Click>
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
        </p>
        <Input value=input_value placeholder="Enter an announce address">
            <InputPrefix slot>
                <Icon icon=icondata::AiUserOutlined />
            </InputPrefix>
        </Input>
        <Button on:click=add_peer>Add peer</Button>
        {show_pending_peers}
        <h2 class="text-xl">"Connected peers"</h2>
        {show_peers}
    }
}
