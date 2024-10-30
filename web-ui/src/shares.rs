use super::{peer::Peer, Command, ErrorMessage, RequesterSetter, SuccessMessage, BUTTON_STYLE};
use leptos::{html::Input, *};

#[component]
pub fn Shares(
    shares: ReadSignal<Option<Peer>>,
    add_or_remove_share_message: ReadSignal<Option<Result<String, String>>>,
    home_dir: ReadSignal<Option<String>>,
) -> impl IntoView {
    let selves = move || match shares.get() {
        Some(shares) => vec![shares],
        None => Vec::new(),
    };

    let input_ref = create_node_ref::<Input>();
    let set_requester = use_context::<RequesterSetter>().unwrap().0;

    let home_dir_if_exists = move || {
        let home_dir_option = home_dir.get();
        match home_dir_option {
            Some(h) => h,
            None => Default::default(),
        }
    };

    let add_share = move |_| {
        let input = input_ref.get().unwrap();
        let dir_to_share = input.value();
        let dir_to_share = dir_to_share.trim();
        if !dir_to_share.is_empty() {
            let join = Command::AddShare(dir_to_share.to_string());
            set_requester.update(|requester| requester.make_request(join));
        }
        input.set_value(&home_dir_if_exists());
    };

    view! {
        <h2 class="text-xl">"Shared files"</h2>
        <form action="javascript:void(0);">
            <label for="add-share">"Add a directory to share"</label>
            <div>
                <code>
                    <input
                        value=home_dir_if_exists
                        class="border-2 mx-1"
                        name="add-share"
                        node_ref=input_ref
                    />
                </code>
                <input type="submit" value="Add" class=BUTTON_STYLE on:click=add_share value="Add"/>
            </div>
        </form>

        // TODO could use <Show> here
        {move || {
            match add_or_remove_share_message.get() {
                Some(Ok(message)) => {
                    view! {
                        <span>
                            <SuccessMessage message/>
                        </span>
                    }
                }
                Some(Err(message)) => {
                    view! {
                        <span>
                            <ErrorMessage message/>
                        </span>
                    }
                }
                None => {
                    view! { <span></span> }
                }
            }
        }}

        <ul class="list-disc list-inside">
            <For
                each=selves
                key=|peer| format!("{}{}", peer.name, peer.files.len())
                children=move |peer| view! { <Peer peer/> }
            />
        </ul>
    }
}
