use crate::{PeerPath, UiDownloadRequest};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct Requests(BTreeMap<(u64, u32), PeerPath>);

impl Requests {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn insert(&mut self, request: &UiDownloadRequest) -> Option<PeerPath> {
        let peer_path = PeerPath {
            peer_name: request.peer_name.clone(),
            path: request.path.clone(),
        };
        self.0
            .insert((request.timestamp.as_secs(), request.request_id), peer_path)
    }

    pub fn get_by_id(&self, id: u32) -> Option<&PeerPath> {
        self.0.iter().find(|(k, _v)| k.1 == id).map(|(_k, v)| v)
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, (u64, u32), PeerPath> {
        self.0.iter()
    }
}
