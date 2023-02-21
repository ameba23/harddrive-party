use clap::{Parser, Subcommand};
use colored::Colorize;
use harddrive_party::{
    hdp::Hdp,
    ui_messages::{Command, UiResponse},
    wire_messages::{LsResponse, Request},
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
    /// Start the process
    Start {
        storage: String,
        share_dir: String,
        ws_addr: Option<SocketAddr>,
    },
    /// Connect to a peer
    Connect { addr: SocketAddr },
    /// Query remote peers
    Ls {
        path: Option<String>,
        searchterm: Option<String>,
        recursive: Option<bool>,
        peer: Option<String>,
    },
    Read {
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        peer: Option<String>,
    },
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
        } => {
            let ws_addr = ws_addr.unwrap_or_else(|| "127.0.0.1:5001".parse().unwrap());
            let (mut hdp, recv) = Hdp::new(storage, vec![&share_dir]).await.unwrap();
            println!("Listening on {}", hdp.endpoint.local_addr().unwrap());
            let command_tx = hdp.command_tx.clone();

            tokio::spawn(async move {
                harddrive_party::ws::server(ws_addr, command_tx, recv)
                    .await
                    .unwrap();
            });

            hdp.run().await;
        }
        CliCommand::Connect { addr } => {
            // let command = Command::Request(
            //     Request::Ls {
            //         path: None,
            //         searchterm: None,
            //         recursive: false,
            //     },
            //     "".to_string(),
            // );
            harddrive_party::ws::single_client_command(ui_addr, Command::Connect(addr)).await?;
        }
        CliCommand::Ls {
            path,
            searchterm,
            recursive,
            peer,
        } => {
            // TODO If a path is given, convert to pathbuf, split into peername and path components
            let mut responses = harddrive_party::ws::single_client_command(
                ui_addr,
                Command::Request(
                    Request::Ls {
                        path,
                        searchterm,
                        recursive: recursive.unwrap_or(true),
                    },
                    peer.unwrap_or_default(),
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
        CliCommand::Read {
            path,
            start,
            end,
            peer,
        } => {
            // TODO If a path is given, convert to pathbuf, split into peername and path components
            let mut responses = harddrive_party::ws::single_client_command(
                ui_addr,
                Command::Request(Request::Read { path, start, end }, peer.unwrap_or_default()),
            )
            .await?;
            while let Some(response) = responses.recv().await {
                match response {
                    Ok(UiResponse::Read(data)) => {
                        println!("{:?}", data);
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

fn path_to_peer_path(path: String) -> (String, String) {
    let path_buf = PathBuf::from(path);
    if let Some(first_component) = path_buf.iter().next() {
        let peer_name = first_component.to_str().unwrap();
        let remaining_path = path_buf
            .strip_prefix(peer_name)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        (peer_name.to_string(), remaining_path)
    } else {
        ("".to_string(), "".to_string())
    }
}
