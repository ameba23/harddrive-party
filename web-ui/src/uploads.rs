use crate::display_bytes;
use harddrive_party_shared::ui_messages::UploadInfo;
use leptos::prelude::*;
use std::collections::BTreeMap;
use thaw::*;

#[derive(Clone)]
pub struct Uploads(BTreeMap<(String, String), RwSignal<UploadInfo>>);

impl Uploads {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn upsert(&mut self, upload: UploadInfo) {
        let key = (upload.peer_name.clone(), upload.path.clone());
        if let Some(existing) = self.0.get(&key) {
            existing.set(upload);
        } else {
            self.0.insert(key, RwSignal::new(upload));
        }
    }

    pub fn iter(
        &self,
    ) -> std::collections::btree_map::Values<'_, (String, String), RwSignal<UploadInfo>> {
        self.0.values()
    }
}

#[component]
pub fn UploadRow(upload: RwSignal<UploadInfo>) -> impl IntoView {
    let upload_data = move || upload.get();
    let upload_size = Memo::new(move |_| upload.get().total_size);
    let progress = Memo::new(move |_| {
        let info = upload.get();
        match upload_size.get() {
            0 => 0.0,
            size => info.bytes_read as f64 / size as f64,
        }
    });
    view! {
        <div class="upload-row">
            <Icon icon=icondata::LuArrowUp />
            " "
            <span>{move || upload_data().peer_name}</span>
            " "
            <code>{move || upload_data().path}</code>
            " "
            <span>{move || display_bytes(upload_data().bytes_read)}</span>
            " "
            <span>{move || format!("{}/s", display_bytes(upload_data().speed as u64))}</span>
            " "
            <ProgressBar value=progress />
            " "
            <span>
                {move || {
                    let info = upload_data();
                    let total = upload_size.get();
                    format!(
                        "Uploading {} of {} bytes",
                        info.bytes_read,
                        total
                    )
                }}
            </span>
        </div>
    }
}
