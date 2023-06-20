
![](./web-ui/public/img/hdd.png)

## harddrive-party

Allows two or more peers to share files. Peers can choose directories to share, and a 'topic' name to meet at.

Currently **work-in-progress**.

`cargo run -- --verbose start <path to store local db> <path of some files to share>`

## Design goals

- Minimal initial setup - don't need to wait long for shared files to index (no hashing)
- Practical for transferring large media collections
- Remote control via ws / http interface. Can be run on an ARM device or NAS and controlled from another computer
- Minimal reliance on centralised infrastructure - servers are only used for peer discovery
- Offline-first - peers can find each other on local network using mDNS
- Support slow / intermittent connections
- Hackable open source protocol

## Usage

Decide on a 'topic' to meet on. This can be any string. You will be able to connect to anyone who 'joins' the same topic name.

## Protocol

### Peer discovery

Peers discover each other using by announcing themselves using [Waku](https://waku.org) [relay protocol](https://rfc.vac.dev/spec/11), which is based on Libp2p's [gossipsub protocol](https://github.com/libp2p/specs/blob/master/pubsub/gossipsub/README.md).

mDNS is also used to find peers connected to the same local network.

Announcement messages are encrypted using a 'topic name', so it is only possible to find peers who know the topic name. Udp hole-punching is used to connect peers who are behind a NAT or firewall.

### Transport

Peers connect to each other using Quic, with client authentication using ed25519. A Quic stream is opened for each RPC request to a peer. There are two types of request:

- `ls` - for querying the shared file index (with a sub-path, or search term)
- `read` - for downloading a file, or portion of a file. 

[wire messages](./shared/src/wire_messages.rs) are serialized with [bincode](https://docs.rs/bincode)

## Web front end

There is a work-in-progress web front end build with [Leptos](https://docs.rs/leptos) in [`./web-ui`](./web-ui)

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
- [ ] Cancelling downloads / removing items from wishlist
- [ ] Command to disconnect from a peer
- [ ] Allow adding / updating / removing share directories at runtime
- [ ] Search files in web ui
- [ ] Expand / collapse directories in web ui

This is based on a previous NodeJS project - [pegpeg/hdp](https://gitlab.com/pegpeg/hdp) - but has many protocol-level changes.
