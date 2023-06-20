use super::{peer::Peer, Command, ErrorMessage, RequesterSetter, SuccessMessage, BUTTON_STYLE};
use leptos::{html::Input, *};

#[component]
pub fn Shares(
    cx: Scope,
    shares: ReadSignal<Option<Peer>>,
    add_or_remove_share_message: ReadSignal<Option<Result<String, String>>>,
) -> impl IntoView {
    let selves = move || match shares.get() {
        Some(shares) => vec![shares],
        None => Vec::new(),
    };

    let input_ref = create_node_ref::<Input>(cx);
    let set_requester = use_context::<RequesterSetter>(cx).unwrap().0;

    let add_share = move |_| {
        let input = input_ref.get().unwrap();
        let dir_to_share = input.value();
        let dir_to_share = dir_to_share.trim();
        if !dir_to_share.is_empty() {
            let join = Command::AddShare(dir_to_share.to_string());
            set_requester.update(|requester| requester.make_request(join));
        }
        input.set_value("");
    };

    view! { cx,
            <h2 class="text-xl">"Shared files"</h2>
            <form action="javascript:void(0);">
                <label for="add-share">"Add a directory to share"</label>
                <input class="border-2 mx-1" name="add-share" node_ref=input_ref />
                <input type="submit" value="Add" class={ BUTTON_STYLE } on:click=add_share value="Add" />
            </form>
            {
                move || {
                    match add_or_remove_share_message.get() {
                        Some(Ok(message)) => {
                            view! { cx, <span> <SuccessMessage message /> </span> }
                        }
                        Some(Err(message)) => {
                            view! { cx, <span> <ErrorMessage message /> </span> }
                        }
                        None => {
                            view! { cx, <span /> }
                        }
                    }
                }
            }
            <ul class="list-disc list-inside">
                <For
                    each=selves
                    key=|peer| format!("{}{}", peer.name, peer.files.len())
                    view=move |cx, peer| view! { cx,  <Peer peer /> }
                />
            </ul>
    }
}
