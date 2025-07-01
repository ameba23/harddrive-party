use crate::{file::FileDisplayContext, AppContext, File};
use harddrive_party_shared::ui_messages::PeerPath;
use leptos::prelude::*;
use thaw::*;

#[component]
pub fn Search(search_results: ReadSignal<Vec<PeerPath>>) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let input_value = RwSignal::new(String::new());
    let ac_c = app_context.clone();
    let do_search = move |_| {
        let searchterm = input_value.get();
        let searchterm = searchterm.trim();
        if !searchterm.is_empty() {
            ac_c.search(searchterm.to_string());
        }

        input_value.set(String::new());
    };

    let files = app_context.get_files;
    let search_results_iter = move || {
        // Calling .get() clones - we should ideally use .with()
        let search_results = search_results.get();
        search_results.into_iter().filter_map(move |peer_path| {
            let files = files.get();
            match files.get(&peer_path) {
                Some(file) => Some(file.clone()),
                None => None,
            }
        })
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
                    context=FileDisplayContext::SearchResult
                    />
            }
        }
        />
            </TableBody>
            </Table>
    }
}
