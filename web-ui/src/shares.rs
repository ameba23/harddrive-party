use crate::{peer::Peer, ui_messages::Command, ErrorMessage, RequesterSetter, SuccessMessage};
use leptos::{either::EitherOf3, prelude::*};
use thaw::*;

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

    let set_requester = use_context::<RequesterSetter>().unwrap().0;

    let home_dir_if_exists = move || {
        let home_dir_option = home_dir.get();
        match home_dir_option {
            Some(h) => h,
            None => Default::default(),
        }
    };

    view! {
        <h2 class="text-xl">"Shared files"</h2>
        <Flex vertical=true>
            <div>
                {move || {
                    let add_share_value = RwSignal::new(home_dir_if_exists());
                    let add_share = move |_| {
                        let dir_to_share = add_share_value.get();
                        let dir_to_share = dir_to_share.trim();
                        if !dir_to_share.is_empty() {
                            let join = Command::AddShare(dir_to_share.to_string());
                            set_requester.update(|requester| requester.make_request(join));
                        }
                        add_share_value.set(home_dir_if_exists());
                    };

                    view! {
                        <p>"Add a directory to share"</p>
                        <Flex>
                            <Input value=add_share_value>
                                <InputPrefix slot>
                                    <Icon icon=icondata::AiFolderAddOutlined />
                                </InputPrefix>
                            </Input>
                            <Button on:click=add_share>"Add"</Button>
                        </Flex>
                    }
                }}
            </div>

            // TODO could use <Show> here
            {move || {
                match add_or_remove_share_message.get() {
                    Some(Ok(message)) => {
                        EitherOf3::A(
                            view! {
                                <span>
                                    <SuccessMessage message />
                                </span>
                            },
                        )
                    }
                    Some(Err(message)) => {
                        EitherOf3::B(
                            view! {
                                <span>
                                    <ErrorMessage message>
                                        <span />
                                    </ErrorMessage>
                                </span>
                            },
                        )
                    }
                    None => EitherOf3::C(view! { <span></span> }),
                }
            }}

            <For
                each=selves
                key=|peer| format!("{}{}", peer.name, peer.files.len())
                children=move |peer| view! { <Peer peer /> }
            />
        </Flex>
    }
}
