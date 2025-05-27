use crate::{
    discovery::{DiscoveryMethod, PeerConnect},
    hdp::SharedState,
    http::Bincode,
    ui_messages::{FilesQuery, UiResponse, UiServerError},
    wire_messages::{AnnounceAddress, IndexQuery, LsResponse, Request},
};
use async_stream::try_stream;
use axum::{
    body::Body,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use futures::{channel::mpsc, pin_mut, StreamExt};
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};

pub async fn version() -> String {
    "1".to_string()
}

/// POST `/connect`
pub async fn post_connect(
    State(shared_state): State<SharedState>,
    announce_payload: String,
) -> Result<StatusCode, UiServerErrorWrapper> {
    let announce_address_bytes = BASE64_STANDARD_NO_PAD.decode(announce_payload).unwrap();
    let announce_address = AnnounceAddress::from_bytes(announce_address_bytes).unwrap();
    let (response_tx, response_rx) = oneshot::channel();

    let discovery_method = DiscoveryMethod::Direct {
        announce_address: announce_address.clone(),
    };

    let peer_connect = PeerConnect {
        discovery_method,
        response_tx: Some(response_tx),
    };
    //shared_state.
    //now wait for peer_connect event - or a timeout
    Ok(StatusCode::OK)
}

/// POST `/files`
pub async fn post_files(
    State(shared_state): State<SharedState>,
    Bincode(files_query): Bincode<FilesQuery>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    let (mut response_tx, response_rx) = mpsc::channel(256);

    // If no name given send the query to all connected peers
    let requests = match files_query.peer_name {
        Some(name) => {
            vec![(Request::Ls(files_query.query), name)]
        }
        None => {
            let peers = shared_state.peers.lock().await;
            peers
                .keys()
                .map(|peer_name| {
                    (
                        Request::Ls(files_query.query.clone()),
                        peer_name.to_string(),
                    )
                })
                .collect()
        }
    };
    debug!("Making request to {} peers", requests.len());

    // If there is no request to make (no peers), end the response
    // if requests.is_empty() {
    //     return Ok(StatusCode::OK);
    // }

    // Track how many remaining requests there are, so we can terminate the response
    // when all are finished
    let remaining_responses: Arc<Mutex<usize>> = Arc::new(Mutex::new(requests.len()));

    for (request, peer_name) in requests {
        // // First check the local cache for an existing response
        // let mut cache = self.ls_cache.lock().await;
        //
        // if let hash_map::Entry::Occupied(mut peer_cache_entry) = cache.entry(peer_name.clone()) {
        //     let peer_cache = peer_cache_entry.get_mut();
        //     if let Some(responses) = peer_cache.get(&request) {
        //         debug!("Found existing responses in cache");
        //         for entries in responses.iter() {
        //             if self
        //                 .response_tx
        //                 .send(UiServerMessage::Response {
        //                     id,
        //                     response: Ok(UiResponse::Ls(
        //                         LsResponse::Success(entries.to_vec()),
        //                         peer_name.to_string(),
        //                     )),
        //                 })
        //                 .await
        //                 .is_err()
        //             {
        //                 warn!("Response channel closed");
        //                 break;
        //             }
        //         }
        //         // Terminate with an endresponse
        //         // If there was more then one peer we need to only
        //         // send this if we are the last one
        //         let mut remaining = remaining_responses.lock().await;
        //         *remaining -= 1;
        //         if *remaining == 0
        //             && self
        //                 .response_tx
        //                 .send(UiServerMessage::Response {
        //                     id,
        //                     response: Ok(UiResponse::EndResponse),
        //                 })
        //                 .await
        //                 .is_err()
        //         {
        //             warn!("Response channel closed");
        //             break;
        //         }
        //         continue;
        //     }
        // }

        debug!("Sending ls query to {}", peer_name);
        let req_clone = request.clone();
        let peer_name_clone = peer_name.clone();

        match shared_state.request(request, &peer_name).await {
            Ok(recv) => {
                let remaining_responses_clone = remaining_responses.clone();
                // let ls_cache = self.ls_cache.clone();
                let ls_response_stream = {
                    match process_length_prefix(recv).await {
                        Ok(ls_response_stream) => ls_response_stream,
                        Err(error) => {
                            warn!("Could not process length prefix {}", error);
                            return Err(UiServerErrorWrapper::Server(
                                UiServerError::ConnectionError(
                                    "Could not process length prefix".to_string(),
                                ),
                            ));
                        }
                    }
                };
                let mut response_tx = response_tx.clone();
                tokio::spawn(async move {
                    // TODO handle error
                    pin_mut!(ls_response_stream);

                    let mut cached_entries = Vec::new();
                    while let Some(Ok(ls_response)) = ls_response_stream.next().await {
                        // If it is not an err, add it to the local
                        // cache
                        if let LsResponse::Success(entries) = ls_response.clone() {
                            cached_entries.push(entries);
                        }

                        if response_tx
                            .try_send(
                                bincode::serialize(&Ok::<UiResponse, UiServerError>(
                                    UiResponse::Ls(ls_response, peer_name_clone.to_string()),
                                ))
                                .unwrap(),
                            )
                            .is_err()
                        {
                            warn!("Response channel closed");
                            break;
                        }
                    }
                    // if !cached_entries.is_empty() {
                    //     debug!("Writing ls cache {}", cached_entries.len());
                    //     let mut cache = ls_cache.lock().await;
                    //     let peer_cache = cache.entry(peer_name_clone.clone()).or_insert(
                    //         // Unwrap ok here becasue CACHE_SIZE is non-zero
                    //         LruCache::new(NonZeroUsize::new(CACHE_SIZE).unwrap()),
                    //     );
                    //     peer_cache.put(req_clone, cached_entries);
                    // }

                    // Terminate with an endresponse
                    // If there was more then one peer we need to only
                    // send this if we are the last one
                    let mut remaining = remaining_responses_clone.lock().await;
                    *remaining -= 1;
                    if *remaining == 0
                        && response_tx
                            .try_send(
                                bincode::serialize(&Ok::<UiResponse, UiServerError>(
                                    UiResponse::EndResponse,
                                ))
                                .unwrap(),
                            )
                            .is_err()
                    {
                        warn!("Response channel closed");
                    }
                });
            }
            Err(err) => {
                error!("Error from remote peer following ls query {:?}", err);
                // TODO map the error
                if response_tx
                    .try_send(
                        bincode::serialize(&Err::<UiResponse, UiServerError>(
                            UiServerError::RequestError,
                        ))
                        .unwrap(),
                    )
                    .is_err()
                {
                    error!("Channel closed");
                }
            }
        }
    }

    let result_stream = response_rx.map(Ok::<_, UiServerErrorWrapper>);
    Ok((StatusCode::OK, Body::from_stream(result_stream)))
}

/// Query our own share index
/// POST `/shares`
pub async fn post_shares(
    State(shared_state): State<SharedState>,
    Bincode(query): Bincode<IndexQuery>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    match shared_state
        .shares
        .query(query.path, query.searchterm, query.recursive)
    {
        Ok(response_iterator) => {
            let (mut response_tx, response_rx) = mpsc::channel(256);
            tokio::spawn(async move {
                for res in response_iterator {
                    if response_tx
                        .try_send(
                            bincode::serialize(&Ok::<UiResponse, UiServerError>(
                                UiResponse::Shares(res),
                            ))
                            .unwrap(),
                        )
                        .is_err()
                    {
                        warn!("Response channel closed");
                    };
                }
            });

            let result_stream = response_rx.map(Ok::<_, UiServerErrorWrapper>);
            Ok((StatusCode::OK, Body::from_stream(result_stream)))
        }
        Err(error) => {
            warn!("Error querying own shares {:?}", error);
            Err(UiServerErrorWrapper::Server(UiServerError::ShareError(
                error.to_string(),
            )))
        }
    }
}

//         Command::Close => {
//             // TODO tidy up peer discovery / active transfers
//             if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
//                 endpoint.wait_idle().await;
//             }
//             // TODO call flush on sled db
//             return Ok(());
//         }
//         Command::Download { path, peer_name } => {
//             // Get details of the file / dir
//             let ls_request = Request::Ls(IndexQuery {
//                 path: Some(path.clone()),
//                 searchterm: None,
//                 recursive: true,
//             });
//             // let mut cache = self.ls_cache.lock().await;
//             //
//             // if let hash_map::Entry::Occupied(mut peer_cache_entry) =
//             //     cache.entry(peer_name.clone())
//             // {
//             //     let peer_cache = peer_cache_entry.get_mut();
//             //     if let Some(responses) = peer_cache.get(&ls_request) {
//             //         debug!("Found existing responses in cache");
//             //         for entries in responses.iter() {
//             //             for entry in entries.iter() {
//             //                 debug!("Adding {} to wishlist dir: {}", entry.name, entry.is_dir);
//             //             }
//             //         }
//             //     } else {
//             //         debug!("Found nothing in cache");
//             //     }
//             // }
//
//             match self.request(ls_request, &peer_name).await {
//                 Ok(recv) => {
//                     let peer_public_key = {
//                         let peers = self.peers.lock().await;
//                         match peers.get(&peer_name) {
//                             Some(peer) => peer.public_key,
//                             None => {
//                                 warn!("Handling request to download a file from a peer who is not connected");
//                                 if self
//                                     .response_tx
//                                     .send(UiServerMessage::Response {
//                                         id,
//                                         response: Err(UiServerError::RequestError),
//                                     })
//                                     .await
//                                     .is_err()
//                                 {
//                                     return Err(HandleUiCommandError::ChannelClosed);
//                                 } else {
//                                     return Ok(());
//                                 }
//                             }
//                         }
//                     };
//                     let response_tx = self.response_tx.clone();
//                     let wishlist = self.wishlist.clone();
//                     tokio::spawn(async move {
//                         if let Ok(ls_response_stream) = process_length_prefix(recv).await {
//                             pin_mut!(ls_response_stream);
//                             while let Some(Ok(ls_response)) = ls_response_stream.next().await {
//                                 if let LsResponse::Success(entries) = ls_response {
//                                     for entry in entries.iter() {
//                                         if entry.name == path {
//                                             if let Err(err) =
//                                                 wishlist.add_request(&DownloadRequest::new(
//                                                     entry.name.clone(),
//                                                     entry.size,
//                                                     id,
//                                                     peer_public_key,
//                                                 ))
//                                             {
//                                                 error!("Cannot add download request {:?}", err);
//                                             }
//                                         }
//                                         if !entry.is_dir {
//                                             debug!("Adding {} to wishlist", entry.name);
//
//                                             if let Err(err) =
//                                                 wishlist.add_requested_file(&RequestedFile {
//                                                     path: entry.name.clone(),
//                                                     size: entry.size,
//                                                     request_id: id,
//                                                     downloaded: false,
//                                                 })
//                                             {
//                                                 error!(
//                                                     "Cannot make download request {:?}",
//                                                     err
//                                                 );
//                                             };
//                                         }
//                                     }
//                                 }
//                             }
//                             // Inform the UI that the request has been made
//                             if response_tx
//                                 .send(UiServerMessage::Response {
//                                     id,
//                                     response: Ok(UiResponse::Download(DownloadResponse {
//                                         download_info: DownloadInfo::Requested(get_timestamp()),
//                                         path,
//                                         peer_name,
//                                     })),
//                                 })
//                                 .await
//                                 .is_err()
//                             {
//                                 // log error
//                             }
//                         }
//                     });
//                 }
//                 Err(error) => {
//                     error!("Error from remote peer when making query {:?}", error);
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Err(UiServerError::RequestError),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     }
//                 }
//             }
//         }
//         Command::Read(read_query, peer_name) => {
//             let request = Request::Read(read_query);
//
//             match self.request(request, &peer_name).await {
//                 Ok(mut recv) => {
//                     let response_tx = self.response_tx.clone();
//                     tokio::spawn(async move {
//                         let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];
//                         let mut bytes_read: u64 = 0;
//                         // TODO handle errors here
//                         while let Ok(Some(n)) = recv.read(&mut buf).await {
//                             bytes_read += n as u64;
//                             debug!("Read {} bytes", bytes_read);
//
//                             if response_tx
//                                 .send(UiServerMessage::Response {
//                                     id,
//                                     response: Ok(UiResponse::Read(buf[..n].to_vec())),
//                                 })
//                                 .await
//                                 .is_err()
//                             {
//                                 warn!("Response channel closed");
//                                 break;
//                             };
//                         }
//                         // Terminate with an endresponse
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::EndResponse),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             warn!("Response channel closed");
//                         }
//                     });
//                 }
//
//                 Err(err) => {
//                     error!("Error from remote peer following read request {:?}", err);
//                     // TODO map the error
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Err(UiServerError::RequestError),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     }
//                 }
//             }
//         }
//         // Add a directory to share
//         Command::AddShare(share_dir) => {
//             let response_tx = self.response_tx.clone();
//             let mut shares = self.rpc.shares.clone();
//             tokio::spawn(async move {
//                 match shares.scan(&share_dir).await {
//                     Ok(num_added) => {
//                         info!("{} shares added", num_added);
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::AddShare(num_added)),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             error!("Channel closed");
//                         }
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::EndResponse),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             error!("Channel closed");
//                         }
//                     }
//                     Err(err) => {
//                         warn!("Error adding share dir {}", err);
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Err(UiServerError::ShareError(err.to_string())),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             error!("Channel closed");
//                         }
//                     }
//                 };
//             });
//         }
//         Command::RemoveShare(share_name) => {
//             let response_tx = self.response_tx.clone();
//             let mut shares = self.rpc.shares.clone();
//             tokio::spawn(async move {
//                 match shares.remove_share_dir(&share_name) {
//                     Ok(()) => {
//                         info!("{} no longer shared", share_name);
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::EndResponse),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             error!("Channel closed");
//                         }
//                     }
//                     Err(err) => {
//                         warn!("Error removing share dir {}", err);
//                         if response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Err(UiServerError::ShareError(err.to_string())),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             error!("Channel closed");
//                         }
//                     }
//                 };
//             });
//         }
//         Command::RequestedFiles(request_id) => {
//             match self.wishlist.requested_files(request_id) {
//                 Ok(response_iterator) => {
//                     for res in response_iterator {
//                         if self
//                             .response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::RequestedFiles(res)),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             warn!("Response channel closed");
//                             break;
//                         };
//                     }
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Ok(UiResponse::EndResponse),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     }
//                 }
//                 Err(error) => {
//                     error!("Error getting requested files from wishlist {:?}", error);
//                     // TODO more detailed error should be forwarded
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Err(UiServerError::RequestError),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     };
//                 }
//             }
//         }
//         Command::RemoveRequest(_request_id) => {
//             // TODO self.wishlist.remove_request
//             todo!();
//         }
//         Command::Requests => {
//             match self.wishlist.requested() {
//                 Ok(response_iterator) => {
//                     for res in response_iterator {
//                         if self
//                             .response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::Requests(res)),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             warn!("Response channel closed");
//                             break;
//                         };
//                     }
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Ok(UiResponse::EndResponse),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     }
//                 }
//                 Err(error) => {
//                     error!("Error getting requests from wishlist {:?}", error);
//                     // TODO more detailed error should be forwarded
//                     if self
//                         .response_tx
//                         .send(UiServerMessage::Response {
//                             id,
//                             response: Err(UiServerError::RequestError),
//                         })
//                         .await
//                         .is_err()
//                     {
//                         return Err(HandleUiCommandError::ChannelClosed);
//                     };
//                 }
//             }
//         }
//     };
/// An error in response to a UI command
#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub enum UiServerErrorWrapper {
    #[error("Error: {0}")]
    Server(#[from] UiServerError),
}

#[derive(Serialize, Deserialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

impl IntoResponse for UiServerErrorWrapper {
    fn into_response(self) -> Response {
        log::error!("{self:?}");
        let error_response = ErrorResponse {
            error: self.to_string(),
            message: format!("{self:?}"),
        };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)).into_response()
    }
}

/// A stream of Ls responses
type LsResponseStream = futures::stream::BoxStream<'static, anyhow::Result<LsResponse>>;

/// Process responses that are prefixed with their length in bytes
async fn process_length_prefix(mut recv: quinn::RecvStream) -> anyhow::Result<LsResponseStream> {
    // Read the length prefix
    // TODO this should be a varint
    let mut length_buf: [u8; 8] = [0; 8];
    let stream = try_stream! {
        while let Ok(()) = recv.read_exact(&mut length_buf).await {
            let length: u64 = u64::from_le_bytes(length_buf);
            debug!("Read prefix {length}");

            // Read a message
            let length_usize: usize = length.try_into()?;
            let mut msg_buf = vec![Default::default(); length_usize];
            match recv.read_exact(&mut msg_buf).await {
                Ok(()) => {
                    let ls_response: LsResponse = bincode::deserialize(&msg_buf)?;
                    yield ls_response;
                }
                Err(_) => {
                    warn!("Bad prefix / read error");
                    break;
                }
            }
        }
    };
    Ok(stream.boxed())
}
