//! Joining and leaving topics
use crate::{
    ui_messages::{Command, UiTopic},
    RequesterSetter,
};
use leptos::html::Input;
use leptos::{either::Either, prelude::*};

#[component]
pub fn Topics(topics: ReadSignal<Vec<UiTopic>>) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>().unwrap().0;
    let input_ref: NodeRef<Input> = NodeRef::new();

    // When joining a new topic
    let join_topic = move |_| {
        let input = input_ref.get().unwrap();
        let topic_name = input.value();
        let topic_name = topic_name.trim();
        if !topic_name.is_empty() {
            let join = Command::Join(topic_name.to_string());
            set_requester.update(|requester| requester.make_request(join));
        }

        input.set_value("");
    };

    view! {
        <h2 class="text-xl">"Connected topics"</h2>
        <form action="javascript:void(0);">
            <input class="border-2 mx-1" node_ref=input_ref placeholder="Enter a topic name"/>
            <input type="submit" value="Join" on:click=join_topic/>
        </form>
        <h2>"Connected"</h2>
        <ul>
            <For
                each=move || {
                    topics.get().into_iter().filter(|topic| topic.connected).collect::<Vec<_>>()
                }

                key=|topic: &UiTopic| topic.name.clone()
                children=move |topic| {
                    view! {
                        <Topic topic=RwSignal::new(topic) />
                    }
                }
            />

        </ul>
        <h2>"Not connected"</h2>
        <ul>
            <For
                each=move || {
                    topics
                        .get()
                        .into_iter()
                        .filter(|topic| !topic.connected)
                        .collect::<Vec<_>>()
                }

                key=|topic: &UiTopic| topic.name.clone()
                children=move |topic| {
                    view! {
                        <Topic topic=RwSignal::new(topic) />
                    }
                }
            />
        </ul>
    }
}

#[component]
pub fn Topic(topic: RwSignal<UiTopic>) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>().unwrap().0;

    let join_or_leave_button = move || {
        let leave_topic = move |_| {
            let leave = Command::Leave(topic.get().name.to_string());
            set_requester.update(|requester| requester.make_request(leave));
        };

        let join_topic = move |_| {
            let leave = Command::Join(topic.get().name.to_string());
            set_requester.update(|requester| requester.make_request(leave));
        };

        if topic.get().connected {
            Either::Left(view! { <button on:click=leave_topic>"Leave"</button> })
        } else {
            Either::Right(view! { <button on:click=join_topic>"Join"</button> })
        }
    };

    view! {
        <li>
            <code>{topic.get().name}</code>
            {join_or_leave_button}
        </li>
    }
}
