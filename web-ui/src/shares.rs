use super::peer::Peer;
use leptos::*;

#[component]
pub fn Shares(cx: Scope, shares: ReadSignal<Option<Peer>>) -> impl IntoView {
    let selves = move || match shares.get() {
        Some(shares) => vec![shares],
        None => Vec::new(),
    };

    // TODO A form offerring to share another directory

    view! { cx,
            <h2 class="text-xl">"Shared files"</h2>
            <ul class="list-disc list-inside">
                <For
                    each=selves
                    key=|peer| format!("{}{}", peer.name, peer.files.len())
                    view=move |cx, peer| view! { cx,  <Peer peer /> }
                />
            </ul>
    }
}
