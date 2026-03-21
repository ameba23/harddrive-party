use harddrive_party_shared::wire_messages::AnnounceAddress;
use leptos::{either::Either, prelude::*};
use thaw::*;

#[component]
pub fn AnnounceAddressView(announce_address: String) -> impl IntoView {
    match AnnounceAddress::from_string(announce_address.clone()) {
        Ok(addr) => {
            Either::Left(view! {
                <span>
                    <Icon icon=icondata::AiUserOutlined />
                    " "
                    <span>{format!("{} {}", addr.name, addr.connection_details)}</span>
                </span>
            })
        }
        Err(_) => Either::Right(view! { <span><code>{announce_address}</code></span> }),
    }
}
