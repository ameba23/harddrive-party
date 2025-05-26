// /// Handle a command from the UI
// /// This should only return fatal errors - errors relating to handling the command should be
// /// sent to the UI
// async fn handle_command(
//     &mut self,
//     ui_client_message: UiClientMessage,
// ) -> Result<(), HandleUiCommandError> {
//     let id = ui_client_message.id;
//     match ui_client_message.command {
//         Command::Close => {
//             // TODO tidy up peer discovery / active transfers
//             if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
//                 endpoint.wait_idle().await;
//             }
//             // TODO call flush on sled db
//             return Ok(());
//         }
//         Command::Ls(query, peer_name_option) => {
//             // If no name given send the query to all connected peers
//             let requests = match peer_name_option {
//                 Some(name) => {
//                     vec![(Request::Ls(query), name)]
//                 }
//                 None => {
//                     let peers = self.peers.lock().await;
//                     peers
//                         .keys()
//                         .map(|peer_name| (Request::Ls(query.clone()), peer_name.to_string()))
//                         .collect()
//                 }
//             };
//             debug!("Making request to {} peers", requests.len());
//
//             // If there is no request to make (no peers), end the response
//             if requests.is_empty()
//                 && self
//                     .response_tx
//                     .send(UiServerMessage::Response {
//                         id,
//                         response: Ok(UiResponse::EndResponse),
//                     })
//                     .await
//                     .is_err()
//             {
//                 warn!("Response channel closed");
//             }
//
//             // Track how many remaining requests there are, so we can terminate the reponse
//             // when all are finished
//             let remaining_responses: Arc<Mutex<usize>> = Arc::new(Mutex::new(requests.len()));
//
//             for (request, peer_name) in requests {
//                 // First check the local cache for an existing response
//                 let mut cache = self.ls_cache.lock().await;
//
//                 if let hash_map::Entry::Occupied(mut peer_cache_entry) =
//                     cache.entry(peer_name.clone())
//                 {
//                     let peer_cache = peer_cache_entry.get_mut();
//                     if let Some(responses) = peer_cache.get(&request) {
//                         debug!("Found existing responses in cache");
//                         for entries in responses.iter() {
//                             if self
//                                 .response_tx
//                                 .send(UiServerMessage::Response {
//                                     id,
//                                     response: Ok(UiResponse::Ls(
//                                         LsResponse::Success(entries.to_vec()),
//                                         peer_name.to_string(),
//                                     )),
//                                 })
//                                 .await
//                                 .is_err()
//                             {
//                                 warn!("Response channel closed");
//                                 break;
//                             }
//                         }
//                         // Terminate with an endresponse
//                         // If there was more then one peer we need to only
//                         // send this if we are the last one
//                         let mut remaining = remaining_responses.lock().await;
//                         *remaining -= 1;
//                         if *remaining == 0
//                             && self
//                                 .response_tx
//                                 .send(UiServerMessage::Response {
//                                     id,
//                                     response: Ok(UiResponse::EndResponse),
//                                 })
//                                 .await
//                                 .is_err()
//                         {
//                             warn!("Response channel closed");
//                             break;
//                         }
//                         continue;
//                     }
//                 }
//
//                 debug!("Sending ls query to {}", peer_name);
//                 let req_clone = request.clone();
//                 let peer_name_clone = peer_name.clone();
//
//                 match self.request(request, &peer_name).await {
//                     Ok(recv) => {
//                         let response_tx = self.response_tx.clone();
//                         let remaining_responses_clone = remaining_responses.clone();
//                         let ls_cache = self.ls_cache.clone();
//                         let ls_response_stream = {
//                             match process_length_prefix(recv).await {
//                                 Ok(ls_response_stream) => ls_response_stream,
//                                 Err(error) => {
//                                     warn!("Could not process length prefix {}", error);
//                                     return Err(HandleUiCommandError::ConnectionClosed);
//                                 }
//                             }
//                         };
//                         tokio::spawn(async move {
//                             // TODO handle error
//                             pin_mut!(ls_response_stream);
//
//                             let mut cached_entries = Vec::new();
//                             while let Some(Ok(ls_response)) = ls_response_stream.next().await {
//                                 // If it is not an err, add it to the local
//                                 // cache
//                                 if let LsResponse::Success(entries) = ls_response.clone() {
//                                     cached_entries.push(entries);
//                                 }
//
//                                 if response_tx
//                                     .send(UiServerMessage::Response {
//                                         id,
//                                         response: Ok(UiResponse::Ls(
//                                             ls_response,
//                                             peer_name_clone.to_string(),
//                                         )),
//                                     })
//                                     .await
//                                     .is_err()
//                                 {
//                                     warn!("Response channel closed");
//                                     break;
//                                 }
//                             }
//                             if !cached_entries.is_empty() {
//                                 debug!("Writing ls cache {}", cached_entries.len());
//                                 let mut cache = ls_cache.lock().await;
//                                 let peer_cache =
//                                     cache.entry(peer_name_clone.clone()).or_insert(
//                                         // Unwrap ok here becasue CACHE_SIZE is non-zero
//                                         LruCache::new(NonZeroUsize::new(CACHE_SIZE).unwrap()),
//                                     );
//                                 peer_cache.put(req_clone, cached_entries);
//                             }
//
//                             // Terminate with an endresponse
//                             // If there was more then one peer we need to only
//                             // send this if we are the last one
//                             let mut remaining = remaining_responses_clone.lock().await;
//                             *remaining -= 1;
//                             if *remaining == 0
//                                 && response_tx
//                                     .send(UiServerMessage::Response {
//                                         id,
//                                         response: Ok(UiResponse::EndResponse),
//                                     })
//                                     .await
//                                     .is_err()
//                             {
//                                 warn!("Response channel closed");
//                             }
//                         });
//                     }
//                     Err(err) => {
//                         error!("Error from remote peer following ls query {:?}", err);
//                         // TODO map the error
//                         if self
//                             .response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Err(UiServerError::RequestError),
//                             })
//                             .await
//                             .is_err()
//                         {
//                             return Err(HandleUiCommandError::ChannelClosed);
//                         }
//                     }
//                 }
//             }
//         }
//         Command::Shares(query) => {
//             // Query our own share index
//             // TODO should probably do this in a separate task
//             match self
//                 .rpc
//                 .shares
//                 .query(query.path, query.searchterm, query.recursive)
//             {
//                 Ok(response_iterator) => {
//                     for res in response_iterator {
//                         if self
//                             .response_tx
//                             .send(UiServerMessage::Response {
//                                 id,
//                                 response: Ok(UiResponse::Shares(res)),
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
//                     warn!("Error querying own shares {:?}", error);
//                     // TODO send this err to UI
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
//         Command::ConnectDirect(remote_peer) => {
//             if let Err(err) = self
//                 .peer_discovery
//                 .connect_direct_to_peer(&remote_peer, id)
//                 .await
//             {
//                 if self
//                     .response_tx
//                     .send(UiServerMessage::Response {
//                         id,
//                         response: Err(UiServerError::ConnectionError(err.to_string())),
//                     })
//                     .await
//                     .is_err()
//                 {
//                     return Err(HandleUiCommandError::ChannelClosed);
//                 }
//             }
//             // The EndResponse is sent by the connection handler when a connection is established
//         }
//     };
//     Ok(())
// }
