use futures::{
    channel::mpsc::{channel, Receiver},
    stream::{self, LocalBoxStream},
    SinkExt, StreamExt,
};
use harddrive_party_shared::{
    client::ClientError,
    ui_messages::{
        DownloadEvent, DownloadInfo, FilesQuery, Info, PeerPath, UiDownloadRequest, UiEvent,
        UiRequestedFile, UiServerError, UploadInfo,
    },
    wire_messages::{AnnounceAddress, Entry, IndexQuery, LsResponse},
};
use leptos::prelude::document;
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Debug)]
pub enum MockScenario {
    Default,
    Slow,
    Bursty,
    LargeIndex,
    Errory,
}

impl MockScenario {
    fn from_name(value: &str) -> Self {
        match value {
            "slow" => Self::Slow,
            "bursty" => Self::Bursty,
            "large-index" => Self::LargeIndex,
            "errory" => Self::Errory,
            _ => Self::Default,
        }
    }
}

pub fn scenario_from_location() -> Option<MockScenario> {
    let location = document().location()?;
    let query = location.search().ok().unwrap_or_default();
    let query = query.trim_start_matches('?');

    let mut mock_enabled = false;
    let mut scenario = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "mock" => mock_enabled = value == "1" || value.eq_ignore_ascii_case("true"),
            "scenario" => scenario = Some(MockScenario::from_name(&value)),
            _ => {}
        }
    }

    if mock_enabled || scenario.is_some() {
        Some(scenario.unwrap_or(MockScenario::Default))
    } else {
        None
    }
}

#[derive(Clone)]
pub struct MockClient {
    scenario: MockScenario,
    next_request_id: Arc<AtomicU32>,
}

impl MockClient {
    pub fn new(scenario: MockScenario) -> Self {
        Self {
            scenario,
            next_request_id: Arc::new(AtomicU32::new(2000)),
        }
    }

    pub async fn info(&self) -> Result<Info, ClientError> {
        Ok(Info {
            name: "mock-ui".to_string(),
            os_home_dir: Some("/home/pumkin".to_string()),
            announce_address: "mock-uiC9Z/AAAB0".to_string(),
        })
    }

    pub async fn shares(
        &self,
        query: IndexQuery,
    ) -> Result<LocalBoxStream<'static, Result<LsResponse, UiServerError>>, ClientError> {
        let entries = apply_query(own_share_entries(), &query);
        Ok(stream::iter(vec![Ok(LsResponse::Success(entries))]).boxed_local())
    }

    pub async fn files(
        &self,
        query: FilesQuery,
    ) -> Result<LocalBoxStream<'static, Result<(LsResponse, String), UiServerError>>, ClientError>
    {
        let peers = ["asphericKingCrab", "lunarTulipOx"];
        let mut responses = Vec::new();
        for peer in peers {
            if let Some(ref requested_peer) = query.peer_name {
                if requested_peer != peer {
                    continue;
                }
            }
            let entries = apply_query(peer_entries(peer, &self.scenario), &query.query);
            responses.push(Ok((LsResponse::Success(entries), peer.to_string())));
        }
        Ok(stream::iter(responses).boxed_local())
    }

    pub async fn download(&self, _peer_path: &PeerPath) -> Result<u32, ClientError> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    pub async fn connect(&self, _announce_address: String) -> Result<(), ClientError> {
        Ok(())
    }

    pub async fn disconnect(&self, _peer_name: String) -> Result<(), ClientError> {
        Ok(())
    }
    pub async fn known_peers(&self) -> Result<Vec<AnnounceAddress>, ClientError> {
        Ok(vec![
            AnnounceAddress::from_string("asphericKingCrabEJLLAHEK2".to_string())?,
            AnnounceAddress::from_string("lunarTulipOxxjNkTQ1".to_string())?,
            AnnounceAddress::from_string("amberCloudYakG1/LAHFY0".to_string())?,
            AnnounceAddress::from_string("cinderDeltaFoxxjNkyQ1".to_string())?,
        ])
    }

    pub async fn requested_files(
        &self,
        request_id: u32,
    ) -> Result<LocalBoxStream<'static, Result<Vec<UiRequestedFile>, UiServerError>>, ClientError>
    {
        let files = vec![
            UiRequestedFile {
                path: "film/trailer.mov".to_string(),
                size: 3 * 1024 * 1024 * 1024,
                request_id,
                downloaded: false,
            },
            UiRequestedFile {
                path: "film/notes.txt".to_string(),
                size: 4096,
                request_id,
                downloaded: true,
            },
        ];
        Ok(stream::iter(vec![Ok(files)]).boxed_local())
    }

    pub async fn requests(
        &self,
    ) -> Result<LocalBoxStream<'static, Result<Vec<UiDownloadRequest>, UiServerError>>, ClientError>
    {
        Ok(stream::iter(vec![Ok(vec![UiDownloadRequest {
            path: "film/trailer.mov".to_string(),
            progress: 0,
            total_size: 3 * 1024 * 1024 * 1024,
            request_id: 2000,
            timestamp: Duration::from_secs(1_710_000_000),
            peer_name: "asphericKingCrab".to_string(),
        }])])
        .boxed_local())
    }

    pub async fn add_share(&self, _share_dir: String) -> Result<u32, ClientError> {
        Ok(42)
    }

    pub async fn remove_share(&self, _share_dir: String) -> Result<(), ClientError> {
        Ok(())
    }
}

pub fn start_mock_events(scenario: MockScenario) -> Receiver<UiEvent> {
    let (mut out_tx, out_rx) = channel::<UiEvent>(1024);
    spawn_local(async move {
        let _ = out_tx
            .send(UiEvent::PeerConnected {
                name: "asphericKingCrab".to_string(),
            })
            .await;
        let _ = out_tx
            .send(UiEvent::PeerConnected {
                name: "lunarTulipOx".to_string(),
            })
            .await;

        let mut upload_bytes = 0u64;
        let mut download_bytes = 0u64;
        let mut tick = 0u64;
        let total_bytes = 4 * 1024 * 1024 * 1024;

        loop {
            tick = tick.saturating_add(1);
            let interval_ms = match scenario {
                MockScenario::Slow => 800,
                MockScenario::Bursty => 120,
                _ => 300,
            };
            gloo_timers::future::sleep(Duration::from_millis(interval_ms)).await;

            let upload_step = match scenario {
                MockScenario::Slow => 512 * 1024,
                MockScenario::Bursty => 8 * 1024 * 1024,
                _ => 2 * 1024 * 1024,
            };
            upload_bytes = (upload_bytes + upload_step).min(total_bytes);
            if out_tx
                .send(UiEvent::Uploaded(UploadInfo {
                    path: "/home/pumkin/video-archive.mkv".to_string(),
                    bytes_read: upload_bytes,
                    total_size: total_bytes,
                    speed: upload_step as usize,
                    peer_name: "lunarTulipOx".to_string(),
                }))
                .await
                .is_err()
            {
                break;
            }

            let download_step = match scenario {
                MockScenario::Slow => 768 * 1024,
                MockScenario::Bursty => 12 * 1024 * 1024,
                _ => 3 * 1024 * 1024,
            };
            download_bytes = (download_bytes + download_step).min(total_bytes);
            if out_tx
                .send(UiEvent::Download(DownloadEvent {
                    request_id: 2000,
                    path: "film/trailer.mov".to_string(),
                    peer_name: "asphericKingCrab".to_string(),
                    download_info: DownloadInfo::Downloading {
                        path: "film/trailer.mov".to_string(),
                        bytes_read: download_bytes,
                        total_bytes_read: download_bytes,
                        speed: download_step as u32,
                    },
                }))
                .await
                .is_err()
            {
                break;
            }

            if download_bytes >= total_bytes {
                if out_tx
                    .send(UiEvent::Download(DownloadEvent {
                        request_id: 2000,
                        path: "film/trailer.mov".to_string(),
                        peer_name: "asphericKingCrab".to_string(),
                        download_info: DownloadInfo::Completed(Duration::from_secs(tick)),
                    }))
                    .await
                    .is_err()
                {
                    break;
                }
                download_bytes = 0;
                upload_bytes = 0;
            }

            if matches!(scenario, MockScenario::Errory) && tick % 12 == 0 {
                if out_tx
                    .send(UiEvent::PeerConnectionFailed {
                        name: "lunarTulipOx".to_string(),
                        error: "Simulated timeout".to_string(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    out_rx
}

fn own_share_entries() -> Vec<Entry> {
    vec![
        entry("Music", 667 * 1024 * 1024 * 1024, true),
        entry("Movies", 230 * 1024 * 1024 * 1024, true),
        entry("Music/Breakcore", 110 * 1024 * 1024 * 1024, true),
        entry("Music/Breakcore/live-set.flac", 180 * 1024 * 1024, false),
        entry("Movies/demoscene.mp4", 3 * 1024 * 1024 * 1024, false),
    ]
}

fn peer_entries(peer_name: &str, scenario: &MockScenario) -> Vec<Entry> {
    let mut entries = vec![
        entry("film", 308 * 1024 * 1024 * 1024, true),
        entry("film/trailer.mov", 3 * 1024 * 1024 * 1024, false),
        entry("film/behind-the-scenes.mov", 5 * 1024 * 1024 * 1024, false),
        entry("musique", 667 * 1024 * 1024 * 1024, true),
        entry("musique/02 - Creep.mp3", 3_600_000, false),
    ];
    if peer_name == "lunarTulipOx" {
        entries.push(entry("live", 18 * 1024 * 1024 * 1024, true));
        entries.push(entry("live/2026-02-set.flac", 1_600_000_000, false));
    }
    if matches!(scenario, MockScenario::LargeIndex) {
        for i in 0..5000u32 {
            entries.push(entry(
                &format!("archive/file-{i:05}.bin"),
                16 * 1024 * 1024,
                false,
            ));
        }
    }
    entries
}

fn apply_query(entries: Vec<Entry>, query: &IndexQuery) -> Vec<Entry> {
    let base = query.path.as_deref().unwrap_or_default();
    let searchterm = query.searchterm.as_ref().map(|s| s.to_lowercase());
    entries
        .into_iter()
        .filter(|entry| {
            if base.is_empty() {
                if !query.recursive && entry.name.contains('/') {
                    return false;
                }
            } else if entry.name == base {
                return false;
            } else if let Some(rest) = entry.name.strip_prefix(&(base.to_string() + "/")) {
                if !query.recursive && rest.contains('/') {
                    return false;
                }
            } else {
                return false;
            }

            if let Some(searchterm) = &searchterm {
                entry.name.to_lowercase().contains(searchterm)
            } else {
                true
            }
        })
        .collect()
}

fn entry(name: &str, size: u64, is_dir: bool) -> Entry {
    Entry {
        name: name.to_string(),
        size,
        is_dir,
    }
}
