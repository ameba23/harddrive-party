use crate::{
    display_bytes,
    file::{File, FileDisplayContext},
    ui_messages::Command,
    FilesReadSignal, PeerPath, RequesterSetter,
};
use leptos::{either::Either, html::Input, prelude::*};
use std::collections::{HashMap, HashSet};
use std::ops::Bound::Included;

#[derive(Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub files: HashSet<String>,
    pub is_self: bool,
}

impl Peer {
    pub fn new(name: String, is_self: bool) -> Self {
        Self {
            name,
            files: HashSet::new(),
            is_self,
        }
    }
}

#[component]
pub fn Peer(peer: Peer) -> impl IntoView {
    let files = use_context::<FilesReadSignal>().unwrap().0;
    // This signal is used below to provide context to File
    let (peer_signal, _set_peer) = signal((peer.name.clone(), peer.is_self));

    // This should probably be in a closure
    let root_size = display_bytes(
        match files.get().get(&PeerPath {
            peer_name: peer.name,
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
            .map(|(_, file)| file.clone()) // TODO ideally dont clone
            .collect::<Vec<File>>()
    };

    // provide_context(PeerName(peer_signal));
    view! {
        <li>
            {peer_signal.get().0} " " {root_size} " shared" <table>

                <For
                    each=files_iter
                    key=|file| file.name.clone()
                    children=move |file: File| {
                        view! {
                            <File file is_shared=peer.is_self context=FileDisplayContext::Peer/>
                        }
                    }
                />

            </table>
        </li>
    }
}

#[component]
pub fn Peers(
    peers: ReadSignal<HashMap<String, Peer>>,
    announce_address: ReadSignal<Option<String>>,
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
                    <ul>
                        <For
                            each=move || peers.get()
                            key=|(peer_name, peer)| format!("{}{}", peer_name, peer.files.len())
                            children=move |(_peer_name, peer)| view! { <Peer peer/> }
                        />
                    </ul>
                </div>
            })
        }
    };

    let set_requester = use_context::<RequesterSetter>().unwrap().0;
    let input_ref: NodeRef<Input> = NodeRef::new();
    let add_peer = move |_| {
        let input = input_ref.get().unwrap();
        let announce_payload = input.value();
        let announce_payload = announce_payload.trim();
        if !announce_payload.is_empty() {
            let add = Command::ConnectDirect(announce_payload.to_string());
            set_requester.update(|requester| requester.make_request(add));
        }

        input.set_value("");
    };

    let announce = move || {
        announce_address
            .get()
            .unwrap_or("No announce address".to_string())
    };

    view! {
        <p>Announce address<code>{announce}</code>
        </p>
        <form action="javascript:void(0);">
            <input class="border-2 mx-1" node_ref=input_ref placeholder="Enter an announce address"/>
            <input type="submit" value="Add peer" on:click=add_peer/>
        </form>
        <h2 class="text-xl">"Connected peers"</h2>
        {show_peers}
    }
}
