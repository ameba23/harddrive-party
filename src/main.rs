use clap::{Parser, Subcommand};
use colored::Colorize;
use harddrive_party::{
    hdp::Hdp,
    http::http_server,
    ui_messages::{Command, UiResponse},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
    ws::single_client_command,
};
use std::{net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;

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
        storage: Option<String>,
        share_dir: Option<String>,
        #[arg(short, long)]
        ws_addr: Option<SocketAddr>,
        #[arg(short, long)]
        topic: Option<String>,
        #[arg(short, long)]
        dev: bool,
    },
    /// Join a given topic name
    Join { topic: String },
    /// Leave a given topic name
    Leave { topic: String },
    /// Download a file or dir
    Download { path: String },
    /// Query remote peers' file index
    Ls {
        path: Option<String>,
        searchterm: Option<String>,
        recursive: Option<bool>,
    },
    /// Query your shared files
    Shares {
        path: Option<String>,
        searchterm: Option<String>,
        recursive: Option<bool>,
    },
    /// Read a single remote file directly to stdout
    Read {
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    },
    /// Connect to a peer - directly (temporary)
    Connect { addr: SocketAddr },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let ui_addr = cli
        .ui_addr
        .unwrap_or(format!("ws://{}", DEFAULT_UI_ADDRESS.to_string()));

    if cli.verbose {
        std::env::set_var(
            "RUST_LOG",
            std::env::var_os("RUST_LOG").unwrap_or_else(|| "harddrive_party=debug".into()),
        );
    }
    env_logger::init();

    match cli.command {
        CliCommand::Start {
            storage,
            share_dir,
            ws_addr,
            topic,
            dev,
        } => {
            let ws_addr = ws_addr.unwrap_or_else(|| DEFAULT_UI_ADDRESS.parse().unwrap());

            let mut initial_topics = Vec::new();
            if let Some(t) = topic {
                initial_topics.push(t);
            }

            let storage = storage.map(PathBuf::from).unwrap_or_else(|| {
                if dev {
                    PathBuf::from("harddrive-party")
                } else {
                    let os_home_dir = match std::env::var_os("HOME") {
                        Some(o) => o.to_str().unwrap_or(".").to_string(),
                        None => ".".to_string(),
                    };
                    let mut path_buf = PathBuf::from(os_home_dir);
                    path_buf.push(".harddrive-party");
                    path_buf
                }
            });

            let initial_share_dirs = if let Some(dir) = share_dir {
                vec![dir]
            } else {
                Vec::new()
            };

            match Hdp::new(storage, initial_share_dirs, initial_topics).await {
                Ok((mut hdp, recv)) => {
                    println!(
                        "{} listening for peers on {}",
                        hdp.name.green(),
                        hdp.endpoint.local_addr().unwrap().to_string().yellow(),
                    );

                    let command_tx = hdp.command_tx.clone();

                    let ws_listener = TcpListener::bind(&ws_addr).await?;
                    println!("WS Listening on: {}", ws_addr);
                    tokio::spawn(async move {
                        harddrive_party::ws::server(ws_listener, command_tx, recv)
                            .await
                            .unwrap();
                    });

                    let download_dir = hdp.download_dir.clone();
                    // HTTP server
                    tokio::spawn(async move {
                        http_server(ws_addr, download_dir).await;
                    });

                    hdp.run().await;
                }
                Err(error) => {
                    println!("Error {}", error);
                }
            }
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
        CliCommand::Connect { addr } => {
            harddrive_party::ws::single_client_command(ui_addr, Command::Connect(addr)).await?;
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
