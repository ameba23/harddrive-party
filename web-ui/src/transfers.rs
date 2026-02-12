use crate::{
    file::File,
    requests::{Request, Requests},
    uploads::{UploadRow, Uploads},
    PeerPath,
};
use leptos::prelude::*;
use std::collections::BTreeMap;
use thaw::*;

#[component]
pub fn Transfers(
    requests: ReadSignal<Requests>,
    files: ReadSignal<BTreeMap<PeerPath, File>>,
    uploads: ReadSignal<Uploads>,
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
    let uploads_list = move || uploads.get().iter().cloned().collect::<Vec<_>>();

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
        <h3 class="text-lg">"Uploads"</h3>
        <Flex vertical=true>
            <For
                each=uploads_list
                key=|upload| {
                    let data = upload.get_untracked();
                    format!("{}:{}", data.peer_name, data.path)
                }
                children=move |upload| view! { <UploadRow upload=upload /> }
            />
        </Flex>
    }
}
