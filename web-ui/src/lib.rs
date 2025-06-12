pub mod components;
pub mod file;
pub mod hdp;
pub mod peer;
mod requests;
pub mod shares;
pub mod transfers;
pub mod ws;

use crate::{
    file::File,
    peer::Peer,
    ui_messages::{FilesQuery, UiServerError},
};
use futures::StreamExt;
use harddrive_party_shared::{client::Client, wire_messages::IndexQuery};
pub use hdp::*;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::Router;
use log::{debug, warn};
use std::collections::{BTreeMap, HashMap};
use thaw::*;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <ConfigProvider>
            <Router>
                <HdpUi />
            </Router>
        </ConfigProvider>
    }
}

pub fn shares_query(
    ui_url: url::Url,
    query: IndexQuery,
    own_name: Option<String>,
    set_files: WriteSignal<BTreeMap<PeerPath, File>>,
) {
    spawn_local(async move {
        let client = Client::new(ui_url);
        let mut shares_stream = client.shares(query).await.unwrap();
        debug!("Making shares query {own_name:?}");
        while let Some(response) = shares_stream.next().await {
            match response {
                Ok(ls_response) => match ls_response {
                    LsResponse::Success(entries) => {
                        debug!("processing entries");
                        if let Some(ref own_name) = own_name {
                            set_files.update(|files| {
                                for entry in entries {
                                    let peer_path = PeerPath {
                                        peer_name: own_name.clone(),
                                        path: entry.name.clone(),
                                    };
                                    if !files.contains_key(&peer_path) {
                                        files.insert(
                                            peer_path,
                                            File::from_entry(entry, own_name.clone()),
                                        );
                                    }
                                }
                            });
                        }
                    }
                    LsResponse::Err(err) => {
                        warn!("Responded to shares request with err {:?}", err);
                    }
                },
                Err(e) => {
                    println!("Error from server {:?}", e);
                    break;
                }
            }
        }
    });
}

// pub struct UiClient {
//     pub inner: Client,
//     pub set_peers: WriteSignal<HashMap<String, Peer>>,
//     pub shares: ReadSignal<Option<Peer>>,
//     pub set_shares: WriteSignal<Option<Peer>>,
//     pub set_home_dir: WriteSignal<Option<String>>,
//     pub set_announce_address: WriteSignal<Option<String>>,
//     pub set_files: WriteSignal<BTreeMap<PeerPath, File>>,
//     // pending peers? // HashSet<String>
// }
//
// impl UiClient {
//     pub fn info(&self) {
//         let set_announce_address = self.set_announce_address.clone();
//         let set_home_dir = self.set_home_dir.clone();
//         let set_shares = self.set_shares.clone();
//         let fut = self.inner.info();
//         spawn_local(async move {
//             let info = fut.await.unwrap();
//             set_announce_address.update(|address| *address = Some(info.announce_address));
//             set_home_dir.update(|home_dir| *home_dir = info.os_home_dir);
//             set_shares.update(|shares| match shares {
//                 Some(_) => {}
//                 None => *shares = Some(Peer::new(info.name, true)),
//             });
//         });
//     }
//
//     pub fn files(&self, query: FilesQuery) {
//         let s = self.clone();
//         spawn_local(async move {
//             let mut files_stream = s.inner.files(query).await.unwrap();
//
//             while let Some(response) = files_stream.next().await {
//                 match response {
//                     Ok((ls_response, peer_name)) => match ls_response {
//                         LsResponse::Success(entries) => {
//                             debug!("Processing entrys");
//                             let entries_clone = entries.clone();
//                             s.set_peers.update(|peers| {
//                                 let peer = peers
//                                     .entry(peer_name.clone())
//                                     .or_insert(Peer::new(peer_name.clone(), false));
//                                 for entry in entries_clone {
//                                     peer.files.insert(
//                                         entry.name.clone(),
//                                         // File::new(entry),
//                                     );
//                                 }
//                                 debug!("peers {:?}", peers);
//                             });
//                             s.set_files.update(|files| {
//                                 for entry in entries {
//                                     let peer_path = PeerPath {
//                                         peer_name: peer_name.clone(),
//                                         path: entry.name.clone(),
//                                     };
//                                     files
//                                         .entry(peer_path)
//                                         .and_modify(|file| {
//                                             file.size = Some(entry.size);
//                                             file.is_dir = Some(entry.is_dir);
//                                         })
//                                         .or_insert(File::from_entry(entry, peer_name.clone()));
//                                 }
//                             });
//                         }
//                         LsResponse::Err(err) => {
//                             warn!("Peer responded to ls request with err {:?}", err);
//                         }
//                     },
//                     Err(server_error) => {
//                         warn!("Got error from server {:?}", server_error);
//                         match server_error {
//                             UiServerError::ShareError(error_message) => {
//                                 // set_add_or_remove_share_message
//                                 //     .update(|message| *message = Some(Err(error_message)));
//                             }
//                             UiServerError::ConnectionError(error_message) => {}
//                             _ => {
//                                 warn!("Not handling error");
//                             }
//                         }
//                     }
//                 }
//             }
//         });
//     }
//
//     pub fn shares(&self, query: IndexQuery) {
//         debug!("Making shares query {:?}", query);
//
//         let s = self.clone();
//         spawn_local(async move {
//             let mut shares_stream = s.inner.shares(query).await.unwrap();
//
//             while let Some(response) = shares_stream.next().await {
//                 match response {
//                     Ok(ls_response) => match ls_response {
//                         LsResponse::Success(entries) => {
//                             debug!("processing entrys");
//                             let entries_clone = entries.clone();
//                             s.set_shares.update(|shares_option| {
//                                 if let Some(shares) = shares_option {
//                                     for entry in entries_clone {
//                                         shares.files.insert(
//                                             entry.name.clone(),
//                                             // File::new(entry),
//                                         );
//                                     }
//                                 }
//                             });
//                             if let Some(peer) = s.shares.get_untracked() {
//                                 let peer_name = peer.name.clone();
//                                 s.set_files.update(|files| {
//                                     for entry in entries {
//                                         let peer_path = PeerPath {
//                                             peer_name: peer_name.clone(),
//                                             path: entry.name.clone(),
//                                         };
//                                         if !files.contains_key(&peer_path) {
//                                             files.insert(
//                                                 peer_path,
//                                                 File::from_entry(entry, peer_name.clone()),
//                                             );
//                                         }
//                                     }
//                                 });
//                             }
//                         }
//                         LsResponse::Err(err) => {
//                             warn!("Responded to shares request with err {:?}", err);
//                         }
//                     },
//                     Err(e) => {
//                         println!("Error from server {:?}", e);
//                         break;
//                     }
//                 }
//             }
//         });
//     }
// }
