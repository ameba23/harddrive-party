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
pub fn Peer(cx: Scope, peer: Peer) -> impl IntoView {
    let files = use_context::<FilesReadSignal>(cx).unwrap().0;
    let (peer_signal, _set_peer) = create_signal(cx, (peer.name.clone(), peer.is_self));

    let root_size = match files.get().get(&PeerPath {
        peer_name: peer.name,
        path: "".to_string(),
    }) {
        Some(file) => file.size,
        None => 0,
    };

    let files_iter = move || {
        let files = files.get();
        files
            .range((
                Included(PeerPath {
                    peer_name: peer_signal.get().0,
                    path: "".to_string(),
                }),
                Included(PeerPath {
                    peer_name: peer_signal.get().0,
                    path: "ZZZZZ".to_string(), // TODO
                }),
            ))
            .map(|(_, file)| file.clone()) // TODO ideally dont clone
            .collect::<Vec<File>>()
    };

    provide_context(cx, PeerName(peer_signal));
    view! { cx,
        <li>
        {peer_signal.get().0 } " "
        { display_bytes(root_size) } " shared"
        <table>

                <For
                    each=files_iter
                    key=|file| file.name.clone()
                    view=move |cx, file: File| view! { cx,  <File file is_shared=peer.is_self/> }
                />
                    </table>
        </li>
    }
}

#[component]
pub fn Peers(cx: Scope, peers: leptos::ReadSignal<HashMap<String, Peer>>) -> impl IntoView {
    let show_peers = move || {
        if peers.get().is_empty() {
            view! { cx, <div><p>"No peers connected"</p></div> }
        } else {
            view! { cx,
                <div>
                    <ul>
                        <For
                            each={move || peers.get()}
                            key=|(peer_name, peer)| format!("{}{}", peer_name, peer.files.len())
                            view=move |cx, (_peer_name, peer)| view! { cx,  <Peer peer /> }
                        />
                    </ul>
                </div>
            }
        }
    };

    view! { cx,
            <h2 class="text-xl">"Connected peers"</h2>
            { show_peers() }
    }
}
