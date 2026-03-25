use crate::{file::FileDisplayContext, AppContext, File};
use harddrive_party_shared::ui_messages::PeerPath;
use leptos::{either::Either, prelude::*};
use thaw::*;

#[component]
pub fn Search(search_results: ReadSignal<Vec<PeerPath>>) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let input_value = RwSignal::new(String::new());
    let ac_c = app_context.clone();
    let do_search = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
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

    let show_results = move || {
        if app_context.get_peers.get().is_empty() && search_results.get().is_empty() {
            Either::Left(view! {
                <div>
                    <p>"There are no connected peers - so there is nothing to search"</p>
                </div>
            })
        } else {
            Either::Right(view! {
                <div class="table-scroll">
                    <Table class="search-table">
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
                </div>
            })
        }
    };

    view! {
        <form on:submit=do_search>
            <Flex>
                <Input
                    rules=vec![InputRule::required(true.into())]
                    value=input_value
                    placeholder="Searchterm"
                >
                    <InputPrefix slot>
                        <Icon icon=icondata::ImSearch />
                    </InputPrefix>
                </Input>
                <Button button_type=ButtonType::Submit>
                    "Search"
                </Button>
            </Flex>
        </form>
        {show_results}
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::file::DownloadStatus;
    use leptos::mount::mount_to;
    use leptos::wasm_bindgen::JsCast;
    use thaw::ConfigProvider;
    use web_sys::HtmlElement;
    use wasm_bindgen_test::wasm_bindgen_test;

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn mount_host() -> HtmlElement {
        let document = document();
        let host = document
            .create_element("div")
            .expect("host element should be created")
            .dyn_into::<HtmlElement>()
            .expect("host should be an HtmlElement");
        document
            .body()
            .expect("document body should exist")
            .append_child(&host)
            .expect("host should be appended");
        host
    }

    #[wasm_bindgen_test]
    fn shows_empty_state_when_no_peers_and_no_results() {
        let host = mount_host();
        let app_context = AppContext::for_tests();
        let (search_results, _set_search_results) = signal(Vec::<PeerPath>::new());

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Search search_results />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("There are no connected peers - so there is nothing to search"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn renders_search_results_for_known_files() {
        let host = mount_host();
        let app_context = AppContext::for_tests();
        app_context.set_peers.update(|peers| {
            peers.insert("asphericKingCrab".to_string());
        });

        let peer_path = PeerPath {
            peer_name: "asphericKingCrab".to_string(),
            path: "film/trailer.mov".to_string(),
        };
        app_context.set_files.update(|files| {
            files.insert(
                peer_path.clone(),
                File {
                    name: "film/trailer.mov".to_string(),
                    peer_name: "asphericKingCrab".to_string(),
                    size: Some(1024),
                    is_dir: Some(false),
                    is_expanded: RwSignal::new(false),
                    is_visible: RwSignal::new(true),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
            );
        });
        let (search_results, _set_search_results) = signal(vec![peer_path]);

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Search search_results />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("trailer.mov"));

        drop(handle);
        host.remove();
    }
}
