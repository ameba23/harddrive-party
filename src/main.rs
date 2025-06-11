use anyhow::anyhow;
use clap::{Parser, Subcommand};
use colored::Colorize;
use futures::StreamExt;
use harddrive_party::{
    ui_messages::{DownloadInfo, UiEvent},
    ui_server::{client::Client, http_server},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
    Hdp,
};
use reqwest::Url;
use std::{env, path::PathBuf};
use tokio::fs::create_dir_all;

#[derive(Parser, Debug, Clone)]
#[clap(version, about, long_about = None)]
#[clap(about = "Peer to peer filesharing")]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
    /// Where to host UI, or where to expect it to be hosted
    #[arg(short, long, required = false, default_value = "127.0.0.1:3030")]
    ui_address: String,
    /// Verbose mode with additional logging
    #[arg(short, long)]
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
    },
    /// Download a file or dir
    Download {
        /// Peername and path - given as "peername/path"
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
        /// Peername and path - given as "peername/path"
        path: String,
        /// Offset to start reading at (defaults to beginning of file)
        #[arg(short, long)]
        start: Option<u64>,
        /// Offset to stop reading (defaults to end of file)
        #[arg(short, long)]
        end: Option<u64>,
    },
    /// Connect to a peer
    Connect { announce_address: String },
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

            let mut hdp = Hdp::new(storage, initial_share_dirs, download_dir, !no_mdns).await?;
            println!(
                "{} listening for peers on {}",
                hdp.shared_state.name.green(),
                hdp.server_connection.to_string().yellow(),
            );

            let shared_state = hdp.shared_state.clone();

            let ui_address: std::net::SocketAddr = cli.ui_address.parse()?;
            let addr = http_server(shared_state, ui_address).await?;

            println!("Web UI served on http://{}", addr);

            println!(
                "Announce address {}",
                hdp.shared_state.get_ui_announce_address()
            );

            hdp.run().await;
        }
        CliCommand::Ls {
            path,
            searchterm,
            recursive,
        } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = match path {
                Some(given_path) => {
                    let (peer_name, peer_path) = path_to_peer_path(given_path)?;
                    (peer_name, Some(peer_path))
                }
                None => (None, None),
            };

            let client = Client::new(cli.ui_address.parse()?);
            let mut responses = client
                .files(harddrive_party::ui_messages::FilesQuery {
                    peer_name,
                    query: IndexQuery {
                        path: peer_path,
                        searchterm,
                        recursive: recursive.unwrap_or(true),
                    },
                })
                .await?;

            while let Some(response) = responses.next().await {
                match response {
                    Ok((ls_response, peer_name)) => match ls_response {
                        LsResponse::Success(entries) => {
                            for entry in entries {
                                if entry.is_dir {
                                    println!(
                                        "{} {} bytes",
                                        format!("[{}/{}]", peer_name, entry.name).blue(),
                                        entry.size
                                    );
                                } else {
                                    println!("{}/{} {}", peer_name, entry.name, entry.size);
                                }
                            }
                        }
                        LsResponse::Err(err) => {
                            println!("Error from peer {:?}", err);
                        }
                    },
                    Err(e) => {
                        println!("Error from WS server {:?}", e);
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
                            println!("Error from peer {:?}", err);
                        }
                    },
                    Err(e) => {
                        println!("Error from server {:?}", e);
                        break;
                    }
                }
            }
        }
        CliCommand::Download { path } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = path_to_peer_path(path)?;

            let client = Client::new(cli.ui_address.parse()?);
            let request_id = client
                .download(
                    peer_path,
                    peer_name.ok_or(anyhow!("Peer name must be given"))?,
                )
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
            let (peer_name, peer_path) = path_to_peer_path(path)?;

            let client = Client::new(cli.ui_address.parse()?);
            let mut stream = client
                .read(
                    peer_name.ok_or(anyhow!("Incomplete peer path"))?,
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
            client.connect(announce_address).await?;

            // TODO we need to get peer name from announce address to check this
            // while let Some(event) = client.next().await {
            //     match event? {
            //         UiEvent::PeerConnected(name) => { break; }
            //         UiEvent::PeerConnectionFailed(name, error) => { return Err(anyhow!("{error}")); }
            //     }
            // }
        }
    };
    Ok(())
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
