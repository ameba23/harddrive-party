use async_std::prelude::*;
use harddrive_party::{
    messages::{
        request,
        response::{self, Response},
    },
    rpc::Rpc,
    shares::Shares,
};
use tempfile::TempDir;

mod _testdata;
use _testdata::create_test_entries;

#[async_std::test]
async fn basic_rpc() -> anyhow::Result<()> {
    env_logger::init();
    let storage = TempDir::new().unwrap();
    let mut shares = Shares::new(storage).await.unwrap();
    shares.scan("tests/test-data").await.unwrap();
    let mut rpc = Rpc::new(shares);

    {
        let mut test_entries = create_test_entries();
        let req = request::Msg::Ls(request::Ls {
            path: None,
            searchterm: None,
            recursive: true,
        });
        let mut responses = Box::into_pin(rpc.request(req).await);

        while let Some(res) = responses.next().await {
            match res {
                response::Response::Success(response::Success {
                    msg: Some(response::success::Msg::Ls(response::Ls { entries })),
                }) => {
                    for entry in entries {
                        let i = test_entries.iter().position(|e| e == &entry).unwrap();
                        test_entries.remove(i);
                    }
                }
                response::Response::Err(code) => {
                    panic!("Got error response {}", code);
                }
                _ => {}
            }
        }
        // Make sure we found every entry
        assert_eq!(test_entries.len(), 0);
    }

    {
        // Bad pathname should give an error response
        let req = request::Msg::Ls(request::Ls {
            path: Some("badpath".to_string()),
            searchterm: None,
            recursive: true,
        });
        let mut responses = Box::into_pin(rpc.request(req).await);

        assert_eq!(Some(Response::Err(1)), responses.next().await);
        assert_eq!(None, responses.next().await);
    }

    Ok(())
}
