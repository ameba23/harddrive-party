use harddrive_party_shared::wire_messages::{AnnounceAddress, PeerConnectionDetails};
use leptos::{either::Either, prelude::*};
use thaw::*;

#[component]
pub fn AnnounceAddressView(announce_address: String) -> impl IntoView {
    match AnnounceAddress::from_string(announce_address.clone()) {
        Ok(addr) => {
            let nat_type = match addr.connection_details {
                PeerConnectionDetails::NoNat(_) => "no_nat",
                PeerConnectionDetails::Asymmetric(_) => "asymmetric_nat",
                PeerConnectionDetails::Symmetric(_) => "symmetric_nat",
            };
            let ip = addr.connection_details.ip().to_string();
            let port = addr.connection_details.port();
            let ip_port = match port {
                Some(port) => format!("{ip}:{port}"),
                None => ip.clone(),
            };

            Either::Left(view! {
                <span>
                    <Icon icon=icondata::AiUserOutlined />
                    " "
                    <span>{format!("{} {} {}", addr.name, ip_port, nat_type)}</span>
                </span>
            })
        }
        Err(_) => Either::Right(view! { <span><code>{announce_address}</code></span> }),
    }
}
