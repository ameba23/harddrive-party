## WIP

- Start harddrive-party running at ws://127.0.0.1:4001
- `trunk serve`
- Open browser at localhost:8080

## Mock UI mode

- Run with mock support: `trunk serve --features mock-ui`
- Enable mock mode in the browser: `http://localhost:8080/?mock=1`
- Select scenario: `http://localhost:8080/?mock=1&scenario=slow`

Available scenarios:
- `default`
- `slow`
- `bursty`
- `large-index`
- `errory`

## Frontend tests

- Enter the `web-ui` dev shell so Firefox and `geckodriver` are available: `nix-shell`
- Run the wasm browser tests: `cargo test --target wasm32-unknown-unknown --lib`

These tests use `wasm-bindgen-test` and run headless in Firefox via the Cargo runner configured in the repo.

The `Try find \`webdriver.json\` ... Not found` line in the test output is expected. `webdriver.json` is optional and is only needed if custom WebDriver capabilities are required.

## CI

Yes, these tests can run in CI.

A CI job needs:
- the `wasm32-unknown-unknown` Rust target
- `wasm-bindgen-test-runner`
- Firefox
- `geckodriver`

The CI command can be the same one used locally:
- `cargo test --manifest-path web-ui/Cargo.toml --target wasm32-unknown-unknown --lib`
