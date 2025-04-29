pub mod components;
pub mod file;
pub mod hdp;
pub mod peer;
mod requests;
pub mod shares;
pub mod transfers;
pub mod ws;

pub use hdp::*;
use leptos::prelude::*;
use leptos_router::components::Router;
use thaw::*;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <ConfigProvider>
            <Router>
                <HdpUi/>
            </Router>
        </ConfigProvider>
    }
}
