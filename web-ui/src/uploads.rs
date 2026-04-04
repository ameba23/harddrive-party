use crate::file::{DownloadStatus, File, FileDisplayContext};
use harddrive_party_shared::ui_messages::UploadInfo;
use leptos::prelude::*;
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Uploads(BTreeMap<(String, String), RwSignal<File>>);

impl Uploads {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn upsert(&mut self, upload: UploadInfo) {
        let key = (upload.peer_name.clone(), upload.path.clone());
        if let Some(existing) = self.0.get(&key) {
            existing.update(|file| {
                file.size = Some(upload.total_size);
                file.download_status.set(DownloadStatus::Uploading {
                    bytes_read: upload.bytes_read,
                    total_size: upload.total_size,
                    speed: upload.speed as u64,
                });
            });
        } else {
            self.0.insert(key, RwSignal::new(upload.into()));
        }
    }

    pub fn iter(&self) -> std::collections::btree_map::Values<'_, (String, String), RwSignal<File>> {
        self.0.values()
    }
}

#[component]
pub fn UploadRow(upload: RwSignal<File>) -> impl IntoView {
    view! {
        {move || {
            let file = upload.get();
            view! { <File file is_shared=true context=FileDisplayContext::Transfer /> }
        }}
    }
}
