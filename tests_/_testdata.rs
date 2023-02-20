use harddrive_party::messages::response;

pub fn create_test_entries() -> Vec<response::ls::Entry> {
    vec![
        response::ls::Entry {
            name: "".to_string(),
            size: 17,
            is_dir: true,
        },
        response::ls::Entry {
            name: "test-data".to_string(),
            size: 17,
            is_dir: true,
        },
        response::ls::Entry {
            name: "test-data/subdir".to_string(),
            size: 12,
            is_dir: true,
        },
        response::ls::Entry {
            name: "test-data/subdir/subsubdir".to_string(),
            size: 6,
            is_dir: true,
        },
        response::ls::Entry {
            name: "test-data/somefile".to_string(),
            size: 5,
            is_dir: false,
        },
        response::ls::Entry {
            name: "test-data/subdir/anotherfile".to_string(),
            size: 6,
            is_dir: false,
        },
        response::ls::Entry {
            name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
            size: 6,
            is_dir: false,
        },
    ]
}
