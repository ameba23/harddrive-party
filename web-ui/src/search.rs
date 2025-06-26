use crate::{file::FileDisplayContext, AppContext, File};
use leptos::prelude::*;
use thaw::*;

#[component]
pub fn Search(search_results: ReadSignal<Vec<File>>) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let input_value = RwSignal::new(String::new());
    let do_search = move |_| {
        let searchterm = input_value.get();
        let searchterm = searchterm.trim();
        if !searchterm.is_empty() {
            app_context.search(searchterm.to_string());
        }

        input_value.set(String::new());
    };

    let search_results_iter = move || {
        // Calling .get() clones - we should ideally use .with(|files| files.range...)
        let search_results = search_results.get();
        search_results.into_iter()
    };
    view! {
        <Input value=input_value placeholder="Searchterm">
            <InputPrefix slot>
                <Icon icon=icondata::ImSearch />
            </InputPrefix>
        </Input>
        <Button on:click=do_search>Search</Button>
        <Table>
        <TableBody>
        <For
        each=search_results_iter
        key=|file| file.name.clone()
        children=move |file: File| {
            view! {
                <File
                    file
                    is_shared=false
                    context=FileDisplayContext::Peer
                    />
            }
        }
        />
            </TableBody>
            </Table>
    }
}
