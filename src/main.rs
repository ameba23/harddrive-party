use anyhow::anyhow;
use clap::{Parser, Subcommand};
use colored::Colorize;
use harddrive_party::{
    discovery::DiscoveryMethods,
    hdp::Hdp,
    http::http_server,
    ui_messages::{Command, UiResponse},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
    ws::single_client_command,
};
use std::{env, net::SocketAddr, path::PathBuf};
use tokio::{fs::create_dir_all, net::TcpListener};

const DEFAULT_UI_ADDRESS: &str = "127.0.0.1:4001";

#[derive(Parser, Debug, Clone)]
#[clap(version, about, long_about = None)]
#[clap(about = "Peer to peer filesharing")]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
    #[arg(short, long)]
    ui_addr: Option<String>,
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
        /// Initial topics to join (may be given multiple times)
        #[arg(short, long)]
        topic: Vec<String>,
        /// IP and port to host UI - defaults to 127.0.0.1:4001
        #[arg(short, long)]
        ui_address: Option<SocketAddr>,
        /// Directory to store local database.
        /// Defaults to $XDG_DATA_HOME/harddrive-party or ~/.local/share/harddrive-party
        #[arg(long)]
        storage: Option<String>,
        /// Directory to store downloads. Defaults to ~/Downloads
        #[arg(short, long)]
        download_dir: Option<String>,
    },
    /// Join a given topic name
    Join { topic: String },
    /// Leave a given topic name
    Leave { topic: String },
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let ui_addr = cli
        .ui_addr
        .unwrap_or(format!("ws://{}", DEFAULT_UI_ADDRESS));

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
            ui_address,
            topic,
            download_dir,
        } => {
            let ui_address = ui_address.unwrap_or_else(|| DEFAULT_UI_ADDRESS.parse().unwrap());

            let storage = match storage {
                Some(storage) => PathBuf::from(storage),
                None => {
                    let mut data_dir = get_data_dir()?;
                    data_dir.push("harddrive-party");
                    data_dir
                }
            };

            let initial_topics = topic;
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

            let (mut hdp, recv) = Hdp::new(
                storage,
                initial_share_dirs,
                initial_topics,
                download_dir,
                DiscoveryMethods::MdnsOnly,
            )
            .await?;
            println!(
                "{} listening for peers on {}",
                hdp.name.green(),
                hdp.server_connection.to_string().yellow(),
            );

            let command_tx = hdp.command_tx.clone();

            let ws_listener = TcpListener::bind(&ui_address).await?;
            println!("Websocket server for UI listening on: {}", ui_address);
            tokio::spawn(async move {
                harddrive_party::ws::server(ws_listener, command_tx, recv).await;
            });

            let download_dir = hdp.download_dir.clone();
            // HTTP server
            tokio::spawn(async move {
                http_server(ui_address, download_dir).await;
            });

            hdp.run().await;
        }
        CliCommand::Join { topic } => {
            let mut responses =
                harddrive_party::ws::single_client_command(ui_addr, Command::Join(topic.clone()))
                    .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::EndResponse) => {
                        println!("Successfully joined topic {}", topic);
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                    }
                    Err(e) => {
                        println!("Error when joining topic {:?}", e);
                        break;
                    }
                }
            }
        }
        CliCommand::Leave { topic } => {
            let mut responses =
                harddrive_party::ws::single_client_command(ui_addr, Command::Leave(topic.clone()))
                    .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::EndResponse) => {
                        println!("Successfully left topic {}", topic);
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                    }
                    Err(e) => {
                        println!("Error when leaving topic {:?}", e);
                        break;
                    }
                }
            }
        }
        CliCommand::Ls {
            path,
            searchterm,
            recursive,
        } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = match path {
                Some(given_path) => {
                    let (peer_name, peer_path) = path_to_peer_path(given_path);
                    (peer_name, Some(peer_path))
                }
                None => (None, None),
            };

            let mut responses = harddrive_party::ws::single_client_command(
                ui_addr,
                Command::Ls(
                    IndexQuery {
                        path: peer_path,
                        searchterm,
                        recursive: recursive.unwrap_or(true),
                    },
                    peer_name,
                ),
            )
            .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::Ls(ls_response, peer_name)) => match ls_response {
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
                    Ok(UiResponse::EndResponse) => {
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                    }
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
            let mut responses = harddrive_party::ws::single_client_command(
                ui_addr,
                Command::Shares(IndexQuery {
                    path,
                    searchterm,
                    recursive: recursive.unwrap_or(true),
                }),
            )
            .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::Shares(ls_response)) => match ls_response {
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
                    Ok(UiResponse::EndResponse) => {
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                    }
                    Err(e) => {
                        println!("Error from WS server {:?}", e);
                        break;
                    }
                }
            }
        }
        CliCommand::Download { path } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = path_to_peer_path(path);

            let mut responses = single_client_command(
                ui_addr,
                Command::Download {
                    path: peer_path,
                    peer_name: peer_name.unwrap_or_default(),
                },
            )
            .await?;

            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::Download(download_response)) => {
                        println!("Downloaded {}", download_response);
                    }
                    Ok(UiResponse::EndResponse) => {
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                    }
                    Err(e) => {
                        println!("Error from WS server {:?}", e);
                        break;
                    }
                }
            }
        }
        CliCommand::Read { path, start, end } => {
            // Split path into peername and path components
            let (peer_name, peer_path) = path_to_peer_path(path);

            let mut responses = harddrive_party::ws::single_client_command(
                ui_addr,
                Command::Read(
                    ReadQuery {
                        path: peer_path,
                        start,
                        end,
                    },
                    peer_name.unwrap_or_default(),
                ),
            )
            .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::Read(data)) => {
                        print!("{}", std::str::from_utf8(&data).unwrap());
                    }
                    Ok(UiResponse::EndResponse) => {
                        break;
                    }
                    Ok(some_other_response) => {
                        println!("Got unexpected response {:?}", some_other_response);
                        break;
                    }
                    Err(e) => {
                        println!("Error from WS server {:?}", e);
                        break;
                    }
                }
            }
        }
    };
    Ok(())
}

fn path_to_peer_path(path: String) -> (Option<String>, String) {
    let path_buf = PathBuf::from(path);
    if let Some(first_component) = path_buf.iter().next() {
        let peer_name = first_component.to_str().unwrap();
        let remaining_path = path_buf
            .strip_prefix(peer_name)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        (Some(peer_name.to_string()), remaining_path)
    } else {
        (None, "".to_string())
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
