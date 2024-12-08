use crate::{display_bytes, file::File, FilesReadSignal, PeerName, PeerPath};
use leptos::*;
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
    let (peer_signal, _set_peer) = create_signal((peer.name.clone(), peer.is_self));

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
                    children=move |file: File| view! { <File file is_shared=peer.is_self/> }
                />
            </table>
        </li>
    }
}

#[component]
pub fn Peers(peers: leptos::ReadSignal<HashMap<String, Peer>>) -> impl IntoView {
    let show_peers = move || {
        if peers.get().is_empty() {
            view! {
                <div>
                    <p>"No peers connected"</p>
                </div>
            }
        } else {
            view! {
                <div>
                    <ul>
                        <For
                            each=move || peers.get()
                            key=|(peer_name, peer)| format!("{}{}", peer_name, peer.files.len())
                            children=move |(_peer_name, peer)| view! { <Peer peer/> }
                        />
                    </ul>
                </div>
            }
        }
    };

    view! {
        <h2 class="text-xl">"Connected peers"</h2>
        {show_peers}
    }
}
