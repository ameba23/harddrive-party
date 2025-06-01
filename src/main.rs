use anyhow::anyhow;
use clap::{Parser, Subcommand};
use colored::Colorize;
use harddrive_party::{
    hdp::Hdp,
    http::http_server,
    ui_messages::UiResponse,
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
};
use std::{env, net::SocketAddr, path::PathBuf};
use tokio::{fs::create_dir_all, net::TcpListener};

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

            let ui_address: SocketAddr = cli.ui_address.parse()?;
            let addr = http_server(shared_state, ui_address).await?;

            println!("Web UI served on http://{}", addr);

            println!(
                "Announce address {}",
                hdp.get_announce_address().unwrap_or_default()
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

            // let mut responses = harddrive_party::ws::single_client_command(
            //     ui_addr,
            //     Command::Ls(
            //         IndexQuery {
            //             path: peer_path,
            //             searchterm,
            //             recursive: recursive.unwrap_or(true),
            //         },
            //         peer_name,
            //     ),
            // )
            // .await?;
            // while let Some(response) = responses.recv().await {
            //     match response {
            //         Ok(UiResponse::Ls(ls_response, peer_name)) => match ls_response {
            //             LsResponse::Success(entries) => {
            //                 for entry in entries {
            //                     if entry.is_dir {
            //                         println!(
            //                             "{} {} bytes",
            //                             format!("[{}/{}]", peer_name, entry.name).blue(),
            //                             entry.size
            //                         );
            //                     } else {
            //                         println!("{}/{} {}", peer_name, entry.name, entry.size);
            //                     }
            //                 }
            //             }
            //             LsResponse::Err(err) => {
            //                 println!("Error from peer {:?}", err);
            //             }
            //         },
            //         Ok(UiResponse::EndResponse) => {
            //             break;
            //         }
            //         Ok(some_other_response) => {
            //             println!("Got unexpected response {:?}", some_other_response);
            //         }
            //         Err(e) => {
            //             println!("Error from WS server {:?}", e);
            //             break;
            //         }
            //     }
            // }
        }
        CliCommand::Shares {
            path,
            searchterm,
            recursive,
        } => {
            // let mut responses = harddrive_party::ws::single_client_command(
            //     ui_addr,
            //     Command::Shares(IndexQuery {
            //         path,
            //         searchterm,
            //         recursive: recursive.unwrap_or(true),
            //     }),
            // )
            // .await?;
            // while let Some(response) = responses.recv().await {
            //     match response {
            //         Ok(UiResponse::Shares(ls_response)) => match ls_response {
            //             LsResponse::Success(entries) => {
            //                 for entry in entries {
            //                     if entry.is_dir {
            //                         println!(
            //                             "{} {} bytes",
            //                             format!("[{}]", entry.name).blue(),
            //                             entry.size
            //                         );
            //                     } else {
            //                         println!("{} {}", entry.name, entry.size);
            //                     }
            //                 }
            //             }
            //             LsResponse::Err(err) => {
            //                 println!("Error from peer {:?}", err);
            //             }
            //         },
            //         Ok(UiResponse::EndResponse) => {
            //             break;
            //         }
            //         Ok(some_other_response) => {
            //             println!("Got unexpected response {:?}", some_other_response);
            //         }
            //         Err(e) => {
            //             println!("Error from WS server {:?}", e);
            //             break;
            //         }
            //     }
            // }
        }
        CliCommand::Download { path } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = path_to_peer_path(path)?;

            // let mut responses = single_client_command(
            //     ui_addr,
            //     Command::Download {
            //         path: peer_path,
            //         peer_name: peer_name.unwrap_or_default(),
            //     },
            // )
            // .await?;
            //
            // while let Some(response) = responses.recv().await {
            //     match response {
            //         Ok(UiResponse::Download(download_response)) => {
            //             println!("Downloaded {}", download_response);
            //         }
            //         Ok(UiResponse::EndResponse) => {
            //             break;
            //         }
            //         Ok(some_other_response) => {
            //             println!("Got unexpected response {:?}", some_other_response);
            //         }
            //         Err(e) => {
            //             println!("Error from WS server {:?}", e);
            //             break;
            //         }
            //     }
            // }
        }
        CliCommand::Read { path, start, end } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = path_to_peer_path(path)?;

            // let mut responses = harddrive_party::ws::single_client_command(
            //     ui_addr,
            //     Command::Read(
            //         ReadQuery {
            //             path: peer_path,
            //             start,
            //             end,
            //         },
            //         peer_name.unwrap_or_default(),
            //     ),
            // )
            // .await?;
            // while let Some(response) = responses.recv().await {
            //     match response {
            //         Ok(UiResponse::Read(data)) => {
            //             print!("{}", std::str::from_utf8(&data).unwrap_or_default());
            //         }
            //         Ok(UiResponse::EndResponse) => {
            //             break;
            //         }
            //         Ok(some_other_response) => {
            //             println!("Got unexpected response {:?}", some_other_response);
            //             break;
            //         }
            //         Err(e) => {
            //             println!("Error from WS server {:?}", e);
            //             break;
            //         }
            //     }
            // }
        }
        CliCommand::Connect { announce_address } => {
            // let mut responses = harddrive_party::ws::single_client_command(
            //     ui_addr,
            //     Command::ConnectDirect(announce_address),
            // )
            // .await?;
            // // TODO could add a timeout here
            // while let Some(response) = responses.recv().await {
            //     match response {
            //         Ok(UiResponse::EndResponse) => {
            //             println!("Successfully connected");
            //             break;
            //         }
            //         Ok(some_other_response) => {
            //             println!("Got unexpected response {:?}", some_other_response);
            //         }
            //         Err(e) => {
            //             println!("Error when connecting {:?}", e);
            //             break;
            //         }
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
