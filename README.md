
## harddrive-party

Allows two or more peers to share files. Peers can choose directories to share, and a 'topic' name to meet at.

Currently **work-in-progress**.

`RUST_LOG=harddrive_party=debug cargo run -- start <path to store local db> <path of some files to share>`

## Design goals

- Minimal initial setup - don't need to wait long for files to index
- Practical for transferring large media collections
- Remote control via ws / http interface. Can be run on an ARM device or NAS and controlled from another computer
- Minimal reliance on centralised infrastructure
- Support slow / intermittent connections
- Hackable open source protocol

## Protocol

Peers discover each other using by announcing themselves on an MQTT server, as well as to mDNS for peers connected to the same local network. Announcement messages are encrypted using a 'topic name', so it is only possible to find peers who know the topic name. Udp hole-punching is used to connect peers who are behind a NAT or firewall.

Peers connect to each other using Quic, with client authentication using ed25519. A Quic stream is opened for each RPC request to a peer. There are two types of request:

- `ls` - for querying the shared file index (with a sub-path, or search term)
- `read` - for downloading a file, or portion of a file. 

## Roadmap

- [ ] Extract public key from TLS certificate (rather than just hashing the whole thing)
- [x] Handshaking / prove knowledge of 'topic'
- [x] MQTT should not use `retain` but periodically publish messages
- [x] Support multiple topics / dynamically joining or leaving a topic
- [ ] Offer a DHT as another discovery method (either bittorrent, libp2p or hyperswarm)
- [x] Persistent cryptographic identities / derived peer names
- [x] Query own files
- [ ] Attempt to reconnect to peers after loosing connection
- [x] Holepunching / peer discovery over internet
- [ ] Holepunching for symmetric nat using birthday paradox. [See Tailscale nat traversal article](https://tailscale.com/blog/how-nat-traversal-works)
- [x] Download files rather than just read them
- [x] Handle recursive directory download request
- [x] Queueing / resume downloads
- [ ] Add suitable bounds to channels
- [ ] Web UI (first attempt is here - using Leptos: https://gitlab.com/pegpeg/harddrive-party-web-ui )

This is based on a previous NodeJS project - [pegpeg/hdp](https://gitlab.com/pegpeg/hdp) - but has many protocol-level changes.
