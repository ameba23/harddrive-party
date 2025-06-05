use std::collections::HashMap;

use crate::{
    errors::UiServerErrorWrapper,
    peer::DOWNLOAD_BLOCK_SIZE,
    process_length_prefix,
    ui_messages::{FilesQuery, UiResponse, UiServerError},
    ui_server::Bincode,
    wire_messages::{AnnounceAddress, IndexQuery, LsResponse, Request},
    SharedState,
};
use axum::{
    body::Body,
    extract::{Query, State},
    http::StatusCode,
};
use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use bytes::{BufMut, BytesMut};
use futures::{channel::mpsc, pin_mut, StreamExt};
use harddrive_party_shared::{
    ui_messages::{Info, UiDownloadRequest, UiRequestedFile},
    wire_messages::ReadQuery,
};
use log::{debug, error, warn};
use serde::Serialize;

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

    shared_state.connect_to_peer(announce_address).await?;
    Ok(StatusCode::OK)
}

/// POST `/files`
pub async fn post_files(
    State(shared_state): State<SharedState>,
    Bincode(files_query): Bincode<FilesQuery>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    let (response_tx, response_rx) = mpsc::channel(256);

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
        //         continue;
        //     }
        // }

        debug!("Sending ls query to {}", peer_name);
        let peer_name_clone = peer_name.clone();

        let recv = shared_state.request(request, &peer_name).await?;
        // let ls_cache = self.ls_cache.clone();
        let ls_response_stream = process_length_prefix(recv).await?;

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
                if let Ok(serialized_res) = bincode::serialize(&Ok::<UiResponse, UiServerError>(
                    UiResponse::Ls(ls_response, peer_name_clone.to_string()),
                )) {
                    let serialized_res = create_length_prefixed_message(&serialized_res);
                    if response_tx.try_send(serialized_res).is_err() {
                        warn!("Response channel closed");
                        break;
                    }
                } else {
                    warn!("Could not serialize response");
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
        });
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
    let response_iterator =
        shared_state
            .shares
            .query(query.path, query.searchterm, query.recursive)?;
    stream_response::<LsResponse>(response_iterator).await
}

pub async fn post_download(
    State(shared_state): State<SharedState>,
    // TODO use UI peerpath type
    Bincode((path, peer_name)): Bincode<(String, String)>,
) -> Result<(StatusCode, String), UiServerErrorWrapper> {
    let id = shared_state.download(peer_name, path).await?;
    // TODO consider replacing this reponse with a DownloadResponse struct with timestamp
    //         response: Ok(UiResponse::Download(DownloadResponse {
    //             download_info: DownloadInfo::Requested(get_timestamp()),
    //             path,
    //             peer_name,
    Ok((StatusCode::OK, id.to_string()))
}

pub async fn get_request(
    State(shared_state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    let request_id = params.get("id").unwrap();

    let response_iterator = shared_state
        .wishlist
        .requested_files(request_id.parse().unwrap())?;
    stream_response::<Vec<UiRequestedFile>>(response_iterator).await
}

pub async fn get_requests(
    State(shared_state): State<SharedState>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    let response_iterator = shared_state.wishlist.requested()?;
    stream_response::<Vec<UiDownloadRequest>>(response_iterator).await
}

pub async fn get_info(
    State(shared_state): State<SharedState>,
) -> Result<(StatusCode, Bincode<Info>), UiServerErrorWrapper> {
    Ok((
        StatusCode::OK,
        Bincode(Info {
            announce_address: shared_state.get_ui_announce_address(),
            os_home_dir: shared_state.os_home_dir,
        }),
    ))
}

/// PUT /shares
/// Add a directory to share
/// Returns the number of items added if successful
pub async fn put_shares(
    State(mut shared_state): State<SharedState>,
    share_dir: String,
) -> Result<(StatusCode, String), UiServerErrorWrapper> {
    let num_added = shared_state.shares.scan(&share_dir).await?;
    Ok((StatusCode::OK, num_added.to_string()))
}

/// DELETE /shares
/// Stop sharing a directory
pub async fn delete_shares(
    State(mut shared_state): State<SharedState>,
    share_name: String,
) -> Result<StatusCode, UiServerErrorWrapper> {
    shared_state.shares.remove_share_dir(&share_name)?;
    Ok(StatusCode::OK)
}

/// POST read
/// Directly read a remote peer's file or a portion of a file without downloading it or adding it
/// as a request
/// This is currently not used but could be used for file previews in the UI
/// Returns a raw byte stream with 64kb chunks which may be too big for some clients
pub async fn post_read(
    State(shared_state): State<SharedState>,
    Bincode((read_query, peer_name)): Bincode<(ReadQuery, String)>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper> {
    let request = Request::Read(read_query);
    let mut recv = shared_state.request(request, &peer_name).await?;

    // TODO handle errors here
    let (mut response_tx, response_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        // This is 64kb - which could be too much for some HTTP clients
        let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];
        let mut bytes_read: u64 = 0;

        while let Ok(Some(n)) = recv.read(&mut buf).await {
            bytes_read += n as u64;
            debug!("Read {} bytes", bytes_read);
            if response_tx.try_send(buf[..n].to_vec()).is_err() {
                warn!("Response channel closed - probably the UI client disconnected");
                break;
            };
        }
    });

    let result_stream = response_rx.map(Ok::<_, UiServerErrorWrapper>);
    Ok((StatusCode::OK, Body::from_stream(result_stream)))
}

//         Command::Close => {
//             // TODO tidy up peer discovery / active transfers
//             if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
//                 endpoint.wait_idle().await;
//             }
//             // TODO call flush on sled db
//             return Ok(());
//         }

/// This is used for http responses for the files and shares routes
fn create_length_prefixed_message(message: &[u8]) -> BytesMut {
    let mut buf = BytesMut::with_capacity(4 + message.len());
    buf.put_u32(message.len() as u32); // 4-byte big-endian length prefix
    buf.put_slice(message);
    buf
}

/// Given an iterator of some type, make a streamed HTTP response with length pre-fixed serialized
/// chunks
async fn stream_response<T>(
    input_iterator: Box<dyn Iterator<Item = T> + Send>,
) -> Result<(StatusCode, Body), UiServerErrorWrapper>
where
    T: Serialize + Send + 'static,
{
    let (mut response_tx, response_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        for res in input_iterator {
            match bincode::serialize(&Ok::<T, UiServerError>(res)) {
                Ok(serialized_res) => {
                    let serialized_res = create_length_prefixed_message(&serialized_res);
                    if response_tx.try_send(serialized_res).is_err() {
                        warn!("Response channel closed - probably the UI client disconnected");
                        break;
                    };
                }
                Err(err) => {
                    error!("Could not serialize response: {err}");
                    continue;
                }
            }
        }
    });

    let result_stream = response_rx.map(Ok::<_, UiServerErrorWrapper>);
    Ok((StatusCode::OK, Body::from_stream(result_stream)))
}
