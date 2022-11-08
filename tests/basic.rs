use async_std::prelude::*;
use harddrive_party::{Options, Protocol};

mod _duplex;
use _duplex::Duplex;

#[async_std::test]
async fn basic_protocol() -> anyhow::Result<()> {
    let (ar, bw) = sluice::pipe::pipe();
    let (br, aw) = sluice::pipe::pipe();

    let mut a = Protocol::new(Duplex::new(ar, aw), Options::new(true));
    let mut b = Protocol::new(Duplex::new(br, bw), Options::new(false));
    let next_a = a.next().await;
    println!("a {:?}", next_a);
    let next_b = b.next().await;
    println!("b {:?}", next_b);
    let next_a = a.next().await;
    println!("a {:?}", next_a);

    a.write_ext(vec![20, 20, 20, 20, 20]).await.unwrap();
    let next_a = a.next().await;
    println!("a {:?}", next_a);
    let next_b = b.next().await;
    println!("b {:?}", next_b);
    let next_b = b.next().await;
    println!("b {:?}", next_b);
    return Ok(());
}
