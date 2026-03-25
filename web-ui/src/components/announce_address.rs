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

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use leptos::mount::mount_to;
    use leptos::wasm_bindgen::JsCast;
    use web_sys::HtmlElement;
    use wasm_bindgen_test::wasm_bindgen_test;

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
        let handle = mount_to(host.clone(), || {
            view! { <AnnounceAddressView announce_address="asphericKingCrabEJLLAHEK2".to_string() /> }
        });

        let html = host.inner_html();
        assert!(html.contains("asphericKingCrab"));
        assert!(html.contains("203.0.113.10:4242 Asymmetric NAT"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn falls_back_to_raw_string_for_invalid_announce_address() {
        let host = mount_host();
        let handle = mount_to(host.clone(), || {
            view! { <AnnounceAddressView announce_address="not-a-real-announce-address".to_string() /> }
        });

        let html = host.inner_html();
        assert!(html.contains("<code>not-a-real-announce-address</code>"));

        drop(handle);
        host.remove();
    }
}
