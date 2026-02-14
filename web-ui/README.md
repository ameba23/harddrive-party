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
