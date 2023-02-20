use async_std::prelude::*;
use harddrive_party::{
    messages,
    protocol::{Event, Protocol},
    run::OutGoingPeerRequest,
};
use rand::Rng;

mod _duplex;
use _duplex::Duplex;

#[async_std::test]
async fn basic_protocol() -> anyhow::Result<()> {
    env_logger::init();
    let (ar, bw) = sluice::pipe::pipe();
    let (br, aw) = sluice::pipe::pipe();

    let mut rng = rand::thread_rng();
    let mut a = Protocol::new(Duplex::new(ar, aw), rng.gen(), true);
    let mut b = Protocol::new(Duplex::new(br, bw), rng.gen(), false);

    let (response_tx, response_rx) = async_channel::unbounded();
    a.request(OutGoingPeerRequest {
        response_tx,
        message: messages::request::Msg::Ls(messages::request::Ls {
            path: None,
            searchterm: None,
            recursive: true,
        }),
    })
    .await?;
    loop {
        match a.next().race(b.next()).await {
            Some(Ok(Event::HandshakeResponse)) => {
                println!("handshake response");
            }
            Some(Ok(Event::HandshakeRequest)) => {
                println!("handshake request");
            }
            Some(Ok(Event::Request(req, id))) => {
                println!("Got request {:?} - waiting for response", req);

                let entry = messages::response::ls::Entry {
                    name: String::from("somefile"),
                    size: 1000,
                    is_dir: false,
                };
                let response = messages::response::Response::Success(messages::response::Success {
                    msg: Some(messages::response::success::Msg::Ls(
                        messages::response::Ls {
                            entries: vec![entry],
                        },
                    )),
                });
                b.respond(response, id).await.unwrap();
                let aa = a.next().race(b.next()).await;
                println!("sdlfkj {:?}", aa);
                // let aa = a.next().race(b.next()).await;
                // println!("sdlfkj {:?}", aa);
                // b.next().await;
                // let next_response = response_rx.try_recv().unwrap();
                let next_response = response_rx.recv().await.unwrap();
                println!("Got response {:?}", next_response);
                return Ok(());
            }
            Some(Ok(event)) => {
                println!("Some other event {:?}", event);
            }
            Some(Err(_)) => {
                println!("Err");
            }
            None => {
                println!("None");
            }
        }
    }
}
