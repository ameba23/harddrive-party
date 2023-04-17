use clap::{Parser, Subcommand};
use colored::Colorize;
use harddrive_party::{
    hdp::Hdp,
    ui_messages::{Command, UiResponse},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
    ws::single_client_command,
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser, Debug, Clone)]
#[clap(version, about, long_about = None)]
#[clap(about = "Peer to peer filesharing")]
struct Cli {
    #[clap(subcommand)]
    command: CliCommand,
    #[arg(short, long)]
    ui_addr: Option<String>,
}

#[derive(Subcommand, Debug, Clone)]
enum CliCommand {
    /// Start the process - all other commands will communicate with this instance
    Start {
        storage: String,
        share_dir: String,
        ws_addr: Option<SocketAddr>,
        topic: Option<String>,
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
    env_logger::init();
    let default_ui_addr = "ws://localhost:5001".to_string();
    let cli = Cli::parse();
    let ui_addr = cli.ui_addr.unwrap_or(default_ui_addr);
    match cli.command {
        CliCommand::Start {
            storage,
            share_dir,
            ws_addr,
            topic,
        } => {
            let ws_addr = ws_addr.unwrap_or_else(|| "127.0.0.1:5001".parse().unwrap());

            let mut initial_topics = Vec::new();
            if let Some(t) = topic {
                initial_topics.push(t);
            }

            match Hdp::new(storage, vec![&share_dir], initial_topics).await {
                Ok((mut hdp, recv)) => {
                    println!(
                        "{} listening for peers on {}",
                        hdp.name.green(),
                        hdp.endpoint.local_addr().unwrap().to_string().yellow(),
                    );

                    let command_tx = hdp.command_tx.clone();

                    tokio::spawn(async move {
                        harddrive_party::ws::server(ws_addr, command_tx, recv)
                            .await
                            .unwrap();
                    });

                    hdp.run().await;
                }
                Err(error) => {
                    println!("Error {}", error);
                }
            }
        }
        CliCommand::Join { topic } => {
            harddrive_party::ws::single_client_command(ui_addr, Command::Join(topic)).await?;
            println!("Done joining topic");
        }
        CliCommand::Leave { topic } => {
            harddrive_party::ws::single_client_command(ui_addr, Command::Leave(topic)).await?;
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
