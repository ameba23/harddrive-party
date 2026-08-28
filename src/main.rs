use anyhow::anyhow;
use clap::{Parser, Subcommand};
use colored::Colorize;
use futures::StreamExt;
use harddrive_party::{
    ui_messages::{DownloadInfo, PeerInfo, PeerPath, UiEvent},
    ui_server::{client::Client, http_server},
    wire_messages::{AnnounceAddress, IndexQuery, LsResponse, ReadQuery},
    Hdp,
};
use harddrive_party_shared::PeerId;
use std::{env, net::SocketAddr, path::PathBuf};
use tokio::fs::create_dir_all;
use tokio::signal;

#[derive(Parser, Debug, Clone)]
#[clap(version, about, long_about = None)]
#[clap(about = "Peer to peer filesharing")]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
    /// Where to host UI, or where to expect it to be hosted
    #[arg(
        short,
        long,
        required = false,
        default_value = "http://127.0.0.1:3030",
        global = true
    )]
    ui_address: String,
    /// Verbose mode with additional logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum CliCommand {
    /// Start the process - all other commands will communicate with this instance
    Start {
        /// Directories to share (may be given multiple times)
        #[arg(short, long)]
        share_dir: Vec<String>,
        /// Directory to store local database.
        /// Defaults to $XDG_DATA_HOME/harddrive-party or ~/.local/share/harddrive-party
        #[arg(long)]
        storage: Option<String>,
        /// Directory to store downloads. Defaults to ~/Downloads
        #[arg(short, long)]
        download_dir: Option<String>,
        /// If set, will not use mDNS to discover peers on the local network
        #[arg(long)]
        no_mdns: bool,
        /// Custom STUN server to use for discovery (may be given multiple times)
        #[arg(long, value_name = "HOST:PORT")]
        stun_server: Vec<String>,
        /// Local socket address to listen on for QUIC peer connections.
        /// Defaults to the local IP and previously used port, or an OS-assigned port if none is stored.
        #[arg(long, value_name = "IP:PORT")]
        local_address: Option<SocketAddr>,
    },
    /// Download a file or dir
    Download {
        /// Peer ID (or unambiguous exact display name) and path: "peer/path"
        path: String,
    },
    /// Query remote peers' file index
    Ls {
        /// The directory (defaults to all shared directories)
        path: Option<String>,
        /// A search term to filter by
        #[arg(short, long)]
        searchterm: Option<String>,
        /// Whether to expand subdirectories
        #[arg(short, long)]
        recursive: Option<bool>,
    },
    /// Query your shared files
    Shares {
        /// The directory (defaults to all shared directories)
        path: Option<String>,
        /// A search term to filter by
        #[arg(short, long)]
        searchterm: Option<String>,
        /// Whether to expand subdirectories
        #[arg(short, long)]
        recursive: Option<bool>,
    },
    /// Read a single remote file directly to stdout
    Read {
        /// Peer ID (or unambiguous exact display name) and path: "peer/path"
        path: String,
        /// Offset to start reading at (defaults to beginning of file)
        #[arg(short, long)]
        start: Option<u64>,
        /// Offset to stop reading (defaults to end of file)
        #[arg(short, long)]
        end: Option<u64>,
    },
    /// Connect to a peer
    Connect {
        announce_address: String,
    },
    /// Disconnect from a peer
    Disconnect {
        peer: String,
    },
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        env::set_var(
            "RUST_LOG",
            env::var_os("RUST_LOG").unwrap_or_else(|| "harddrive_party=debug".into()),
        );
    }
    env_logger::init();

    match cli.command {
        CliCommand::Start {
            storage,
            share_dir,
            download_dir,
            no_mdns,
            stun_server,
            local_address,
        } => {
            let storage = match storage {
                Some(storage) => PathBuf::from(storage),
                None => {
                    let mut data_dir = get_data_dir()?;
                    data_dir.push("harddrive-party");
                    data_dir
                }
            };

            let initial_share_dirs = share_dir;

            let download_dir = match download_dir {
                Some(download_dir) => PathBuf::from(download_dir),
                None => {
                    let mut download_dir = get_home_dir()?;
                    download_dir.push("Downloads");
                    download_dir
                }
            };
            create_dir_all(&download_dir).await?;

            let mut hdp = Hdp::new(
                storage,
                initial_share_dirs,
                download_dir,
                !no_mdns,
                local_address,
                if stun_server.is_empty() {
                    None
                } else {
                    Some(stun_server)
                },
            )
            .await?;
            println!(
                "{} listening for peers on {}",
                hdp.shared_state.name.green(),
                hdp.server_connection.to_string().yellow(),
            );

            let shared_state = hdp.shared_state.clone();

            let ui_address = cli
                .ui_address
                .strip_prefix("http://")
                .or_else(|| cli.ui_address.strip_prefix("https://"))
                .unwrap_or(&cli.ui_address);

            let ui_address: std::net::SocketAddr = ui_address.parse()?;
            let addr = http_server(shared_state, ui_address).await?;

            println!("Web UI served on http://{addr}");

            println!(
                "Announce address {}",
                hdp.shared_state.get_ui_announce_address()
            );

            let shared_state = hdp.shared_state.clone();
            tokio::spawn(async move {
                match signal::ctrl_c().await {
                    Ok(()) => {
                        println!("Received Ctrl+C, shutting down...");
                        let force_exit_shared_state = shared_state.clone();
                        tokio::spawn(async move {
                            if signal::ctrl_c().await.is_ok() {
                                eprintln!("Received Ctrl+C again, forcing exit.");
                                force_exit_shared_state
                                    .shutting_down
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                                std::process::exit(130);
                            }
                        });
                        shared_state.shut_down().await;
                    }
                    Err(err) => {
                        eprintln!("Failed to listen for Ctrl+C: {err}");
                    }
                }
            });

            hdp.run().await;
        }
        CliCommand::Ls {
            path,
            searchterm,
            recursive,
        } => {
            // Split path into peername and path components
            let (peer_selector, peer_path) = match path {
                Some(given_path) => {
                    let (peer_name, peer_path) = path_to_peer_path(given_path)?;
                    (peer_name, Some(peer_path))
                }
                None => (None, None),
            };

            let client = Client::new(cli.ui_address.parse()?);
            let peer_id = match peer_selector {
                Some(selector) => Some(resolve_peer(&client, &selector).await?.id),
                None => None,
            };
            let mut responses = client
                .files(harddrive_party::ui_messages::FilesQuery {
                    peer_id,
                    query: IndexQuery {
                        path: peer_path,
                        searchterm,
                        recursive: recursive.unwrap_or(true),
                    },
                })
                .await?;

            while let Some(response) = responses.next().await {
                match response {
                    Ok((ls_response, peer)) => match ls_response {
                        LsResponse::Success(entries) => {
                            for entry in entries {
                                if entry.is_dir {
                                    println!(
                                        "{} {} bytes",
                                        format!("[{}/{}]", display_peer(&peer), entry.name).blue(),
                                        entry.size
                                    );
                                } else {
                                    println!(
                                        "{}/{} {}",
                                        display_peer(&peer),
                                        entry.name,
                                        entry.size
                                    );
                                }
                            }
                        }
                        LsResponse::Err(err) => {
                            println!("Error from peer {err:?}");
                        }
                    },
                    Err(e) => {
                        println!("Error from WS server {e:?}");
                        break;
                    }
                }
            }
        }
        CliCommand::Shares {
            path,
            searchterm,
            recursive,
        } => {
            let client = Client::new(cli.ui_address.parse()?);
            let mut responses = client
                .shares(IndexQuery {
                    path,
                    searchterm,
                    recursive: recursive.unwrap_or(true),
                })
                .await?;

            while let Some(response) = responses.next().await {
                match response {
                    Ok(ls_response) => match ls_response {
                        LsResponse::Success(entries) => {
                            for entry in entries {
                                if entry.is_dir {
                                    println!(
                                        "{} {} bytes",
                                        format!("[{}]", entry.name).blue(),
                                        entry.size
                                    );
                                } else {
                                    println!("{} {}", entry.name, entry.size);
                                }
                            }
                        }
                        LsResponse::Err(err) => {
                            println!("Error from peer {err:?}");
                        }
                    },
                    Err(e) => {
                        println!("Error from server {e:?}");
                        break;
                    }
                }
            }
        }
        CliCommand::Download { path } => {
            // Split path into peername and path components
            let (peer_selector, peer_path) = path_to_peer_path(path)?;

            let client = Client::new(cli.ui_address.parse()?);
            let peer = resolve_peer(
                &client,
                &peer_selector.ok_or(anyhow!("Peer ID or name must be given"))?,
            )
            .await?;
            let request_id = client
                .download(&PeerPath {
                    path: peer_path,
                    peer,
                })
                .await?;
            let mut event_stream = client.event_stream().await?;
            while let Some(event) = event_stream.next().await {
                if let Ok(UiEvent::Download(download_event)) = event {
                    if download_event.request_id == request_id {
                        println!("{download_event:?}");
                        if let DownloadInfo::Completed(_) = download_event.download_info {
                            break;
                        }
                    }
                }
            }
        }
        CliCommand::Read { path, start, end } => {
            // Split path into peername and path components
            let (peer_selector, peer_path) = path_to_peer_path(path)?;

            let client = Client::new(cli.ui_address.parse()?);
            let peer = resolve_peer(
                &client,
                &peer_selector.ok_or(anyhow!("Incomplete peer path"))?,
            )
            .await?;
            let mut stream = client
                .read(
                    peer.id,
                    ReadQuery {
                        path: peer_path,
                        start,
                        end,
                    },
                )
                .await?;

            while let Some(res) = stream.next().await {
                let data = res?;
                print!("{}", std::str::from_utf8(&data).unwrap_or_default());
            }
        }
        CliCommand::Connect { announce_address } => {
            let client = Client::new(cli.ui_address.parse()?);
            let announce_address_parsed = AnnounceAddress::from_string(announce_address.clone())?;

            client.connect(announce_address).await?;

            let mut event_stream = client.event_stream().await?;
            while let Some(event) = event_stream.next().await {
                match event? {
                    UiEvent::PeerConnected { peer }
                        if announce_address_parsed.public_key == peer.id =>
                    {
                        break;
                    }
                    UiEvent::PeerConnectionFailed { peer, error }
                        if announce_address_parsed.public_key == peer.id =>
                    {
                        return Err(anyhow!("{error}"));
                    }
                    _ => {}
                }
            }
        }
        CliCommand::Disconnect { peer } => {
            let client = Client::new(cli.ui_address.parse()?);
            let peer = resolve_peer(&client, &peer).await?;
            let mut event_stream = client.event_stream().await?;
            client.disconnect(peer.id).await?;
            while let Some(event) = event_stream.next().await {
                if let UiEvent::PeerDisconnected {
                    peer: disconnected, ..
                } = event?
                {
                    if disconnected.id == peer.id {
                        break;
                    }
                }
            }
        }
        CliCommand::Stop => {
            let client = Client::new(cli.ui_address.parse()?);
            match client.shut_down().await {
                Ok(()) => {
                    println!("Shut down successfully");
                }
                Err(err) => {
                    println!("Could not gracefully shut down: {err}");
                }
            }
        }
    };
    Ok(())
}

fn display_peer(peer: &PeerInfo) -> String {
    format!("{}#{}", peer.name, peer.id.abbreviated())
}

async fn resolve_peer(client: &Client, selector: &str) -> anyhow::Result<PeerInfo> {
    let peers = client.peers().await?;
    resolve_peer_from(peers, selector)
}

fn resolve_peer_from(peers: Vec<PeerInfo>, selector: &str) -> anyhow::Result<PeerInfo> {
    if let Ok(id) = selector.parse::<PeerId>() {
        return peers
            .into_iter()
            .find(|peer| peer.id == id)
            .ok_or_else(|| anyhow!("Peer {id} is not connected"));
    }

    let matches = peers
        .into_iter()
        .filter(|peer| peer.name == selector)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(anyhow!("No connected peer named {selector:?}")),
        [peer] => Ok(peer.clone()),
        _ => Err(anyhow!(
            "Peer name {selector:?} is ambiguous; matching IDs: {}",
            matches
                .iter()
                .map(|peer| peer.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn path_to_peer_path(path: String) -> anyhow::Result<(Option<String>, String)> {
    let path_buf = PathBuf::from(path.clone());
    if let Some(first_component) = path_buf.iter().next() {
        let peer_name = first_component
            .to_str()
            .ok_or(anyhow!("Could not parse path {path}"))?;
        let remaining_path = path_buf
            .strip_prefix(peer_name)?
            .to_str()
            .ok_or(anyhow!("Could note parse path {path}"))?
            .to_string();
        Ok((Some(peer_name.to_string()), remaining_path))
    } else {
        Ok((None, "".to_string()))
    }
}

/// Get local data directory according to XDG base directory specification
fn get_data_dir() -> anyhow::Result<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(data_dir) => Ok(PathBuf::from(
            data_dir
                .to_str()
                .ok_or(anyhow!("Cannot parse XDG_DATA_HOME"))?,
        )),
        None => {
            let mut data_dir = get_home_dir()?;
            data_dir.push(".local");
            data_dir.push("share");
            Ok(data_dir)
        }
    }
}

/// Gets home directory
fn get_home_dir() -> anyhow::Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home_dir) => Ok(PathBuf::from(
            home_dir.to_str().ok_or(anyhow!("Cannot parse $HOME"))?,
        )),
        None => {
            let username = std::env::var_os("USER").ok_or(anyhow!("Cannot get home directory"))?;
            let username = username.to_str().ok_or(anyhow!("Cannot parse $USER"))?;
            let mut home_dir = PathBuf::from("/home");
            home_dir.push(username);
            Ok(home_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_rejects_ambiguous_names_but_full_ids_work() {
        let first = PeerInfo {
            id: PeerId::new([1; 32]),
            name: "sameAnimal".to_string(),
        };
        let second = PeerInfo {
            id: PeerId::new([2; 32]),
            name: "sameAnimal".to_string(),
        };
        let peers = vec![first.clone(), second.clone()];

        let error = resolve_peer_from(peers.clone(), "sameAnimal").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ambiguous"));
        assert!(message.contains(&first.id.to_string()));
        assert!(message.contains(&second.id.to_string()));

        assert_eq!(
            resolve_peer_from(peers, &second.id.to_string()).unwrap(),
            second
        );
    }

    #[test]
    fn selector_names_are_exact_and_case_sensitive() {
        let peer = PeerInfo {
            id: PeerId::new([3; 32]),
            name: "CaseSensitiveCat".to_string(),
        };
        assert!(resolve_peer_from(vec![peer.clone()], "casesensitivecat").is_err());
        assert_eq!(
            resolve_peer_from(vec![peer.clone()], "CaseSensitiveCat").unwrap(),
            peer
        );
    }
}
