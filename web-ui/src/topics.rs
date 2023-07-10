//! Joining and leaving topics
use crate::{Command, RequesterSetter, BUTTON_STYLE};
use leptos::{html::Input, *};

#[component]
pub fn Topics(cx: Scope, topics: leptos::ReadSignal<Vec<(String, bool)>>) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>(cx).unwrap().0;
    let input_ref = create_node_ref::<Input>(cx);
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

    view! { cx,
        <h2 class="text-xl">"Connected topics"</h2>
        <form action="javascript:void(0);">
            <input class="border-2 mx-1" node_ref=input_ref placeholder="Enter a topic name" />
            <input type="submit" value="Join" class={ BUTTON_STYLE } on:click=join_topic />
        </form>
        <h2>"Connected"</h2>
        <ul>
            <For
                each={move || {
                    topics.get().into_iter().filter(|(_, connected)| *connected).collect::<Vec<_>>()
                } }
                key=|(topic, _): &(String, bool)| topic.clone()
                view=move |cx, (topic, connected) | view! { cx,  <Topic topic=topic.to_string() /> }
            />
        </ul>
        <h2>"Not connected"</h2>
        <ul>
            <For
                each={move || topics.get().into_iter().filter(|(_, connected)| !*connected).collect::<Vec<_>>() }
                key=|(topic, _): &(String, bool)| topic.clone()
                view=move |cx, (topic, connected) | view! { cx,  <Topic topic=topic.to_string() /> }
            />
        </ul>
    }
}

#[component]
pub fn Topic(cx: Scope, topic: String) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>(cx).unwrap().0;
    let topic_clone = topic.clone();
    let leave_topic = move |_| {
        let leave = Command::Leave(topic_clone.to_string());
        set_requester.update(|requester| requester.make_request(leave));
    };

    view! { cx,
        <li>
              <code>{ topic }</code>
              <button class={ BUTTON_STYLE } on:click=leave_topic>
                  "Leave"
              </button>
        </li>
    }
}
