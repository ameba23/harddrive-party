use crate::{
    file::File,
    requests::{Request, Requests},
    PeerPath,
};
use leptos::prelude::*;
use std::collections::BTreeMap;
use thaw::*;

#[component]
pub fn Transfers(
    requests: ReadSignal<Requests>,
    files: ReadSignal<BTreeMap<PeerPath, File>>,
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

    view! {
        <h2 class="text-xl">"Transfers"</h2>
        <h3 class="text-lg">"Requested"</h3>
        <Flex vertical=true>
            <For
                each=wishlist
                key=|file| format!("{}{:?}", file.name, file.size)
                children=move |file| view! { <Request file /> }
            />
        </Flex>
    }
}
