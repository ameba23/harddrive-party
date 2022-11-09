use async_std::prelude::*;
use harddrive_party::{messages, Event, Options, Protocol};

mod _duplex;
use _duplex::Duplex;

#[async_std::test]
async fn basic_protocol() -> anyhow::Result<()> {
    let (ar, bw) = sluice::pipe::pipe();
    let (br, aw) = sluice::pipe::pipe();

    let mut a = Protocol::new(Duplex::new(ar, aw), Options::new(true));
    let mut b = Protocol::new(Duplex::new(br, bw), Options::new(false));
    a.request(messages::request::Msg::Ls(messages::request::Ls {
        path: None,
        searchterm: None,
        recursive: None,
    }))
    .await?;
    loop {
        match a.next().race(b.next()).await {
            Some(Ok(Event::HandshakeResponse)) => {
                println!("handshake response");
            }
            Some(Ok(Event::HandshakeRequest)) => {
                println!("handshake request");
            }
            Some(Ok(event)) => {
                println!("Some other event {:?}", event);
                return Ok(());
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
