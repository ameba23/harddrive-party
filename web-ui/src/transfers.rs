use crate::{
    file::{File, Request},
    PeerPath,
};
use leptos::*;
use std::collections::{BTreeMap, HashSet};

#[component]
pub fn Transfers(
    requested: ReadSignal<HashSet<PeerPath>>,
    downloaded: ReadSignal<HashSet<PeerPath>>,
    files: ReadSignal<BTreeMap<PeerPath, File>>,
) -> impl IntoView {
    let downloaded = move || {
        downloaded
            .get()
            .iter()
            .filter_map(|peer_path| {
                let files = files.get();
                match files.get(peer_path) {
                    Some(file) => Some(file.clone()),
                    None => None,
                }
            })
            .collect::<Vec<File>>()
    };

    let wishlist = move || {
        requested
            .get()
            .iter()
            .filter_map(|peer_path| {
                let files = files.get();
                match files.get(peer_path) {
                    Some(file) => Some(file.clone()),
                    None => None,
                }
            })
            .collect::<Vec<File>>()
    };

    view! {
        <h2 class="text-xl">"Transfers"</h2>
        <h3 class="text-lg">"Requested"</h3>
        <ul class="list-disc list-inside">
            <For
                each=wishlist
                key=|file| format!("{}{}", file.name, file.size)
                children=move |file| view! { <Request file/> }
            />
        </ul>
        <h3 class="text-lg">"Downloaded"</h3>
        <ul class="list-disc list-inside">
            <For
                each=downloaded
                key=|file| format!("{}{}", file.name, file.size)
                children=move |file| view! { <Request file/> }
            />
        </ul>
    }
}
