use harddrive_party_shared::ui_messages::PeerInfo;
use harddrive_party_shared::wire_messages::AnnounceAddress;
use leptos::prelude::*;
use thaw::*;

#[component]
pub fn AnnounceAddressView(announce_address: AnnounceAddress) -> impl IntoView {
    let peer = PeerInfo::from_id(announce_address.public_key);
    let name = format!("{}#{}", peer.name, peer.id.abbreviated());
    let details = announce_address.connection_details.to_string();
    view! {
        <span class="announce-address">
            <Icon icon=icondata::AiUserOutlined />
            <span class="announce-address__name">{name}</span>
            <span class="announce-address__details">{details}</span>
        </span>
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use leptos::mount::mount_to;
    use leptos::wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;
    use web_sys::HtmlElement;

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn mount_host() -> HtmlElement {
        let document = document();
        let host = document
            .create_element("div")
            .expect("host element should be created")
            .dyn_into::<HtmlElement>()
            .expect("host should be an HtmlElement");
        host.set_class_name("test-host");
        document
            .body()
            .expect("document body should exist")
            .append_child(&host)
            .expect("host should be appended");
        host
    }

    #[wasm_bindgen_test]
    fn renders_decoded_announce_address() {
        let host = mount_host();
        let announce_address = AnnounceAddress {
            public_key: harddrive_party_shared::PeerId::new([1; 32]),
            connection_details:
                harddrive_party_shared::wire_messages::PeerConnectionDetails::Asymmetric(
                    "203.0.113.10:4242".parse().unwrap(),
                ),
        };
        let expected_name = PeerInfo::from_id(announce_address.public_key).name;
        let handle = mount_to(host.clone(), || {
            view! {
                <AnnounceAddressView announce_address=announce_address />
            }
        });

        let html = host.inner_html();
        assert!(html.contains(&expected_name));
        assert!(html.contains("203.0.113.10:4242 Asymmetric NAT"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn renders_announced_name_and_connection_details() {
        let host = mount_host();
        let announce_address = AnnounceAddress {
            public_key: harddrive_party_shared::PeerId::new([2; 32]),
            connection_details: harddrive_party_shared::wire_messages::PeerConnectionDetails::NoNat(
                "203.0.113.88:7007".parse().unwrap(),
            ),
        };
        let expected_name = PeerInfo::from_id(announce_address.public_key).name;
        let handle = mount_to(host.clone(), || {
            view! {
                <AnnounceAddressView announce_address=announce_address />
            }
        });

        let html = host.inner_html();
        assert!(html.contains(&expected_name));
        assert!(html.contains("203.0.113.88:7007 No NAT"));

        drop(handle);
        host.remove();
    }
}
