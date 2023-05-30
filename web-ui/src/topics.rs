//! Joining and leaving topics
use crate::{Command, RequesterSetter, BUTTON_STYLE};
use leptos::{html::Input, *};

#[component]
pub fn Topics(cx: Scope, topics: leptos::ReadSignal<Vec<String>>) -> impl IntoView {
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
        <input class="border-2 mx-1" node_ref=input_ref />
        <button class={ BUTTON_STYLE } on:click=join_topic>
            "Join"
        </button>
        <ul>
            <For
                each={move || topics.get()}
                key=|topic| topic.clone()
                view=move |cx, topic| view! { cx,  <Topic topic /> }
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
