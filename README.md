
## harddrive-party

Allows two or more peers to share files. Peers can choose directories to share, and a 'topic' name to meet at.

Currently **work-in-progress**.

## Design goals

- Minimal initial setup - don't need to wait long for files to index
- Practical for transferring large media collections
- Remote control via ws / http interface. Can be run on an ARM device or NAS and controlled from another computer
- Minimal reliance on centralised infrastructure
- Support slow / intermittent connections

## Protocol

Peers discover each other using by announcing themselves on an MQTT server, as well as to mDNS for peers connected to the same local network. Announcement messages are encrypted using a 'topic name', so it is only possible to find peers who know the topic name. Udp hole-punching is used to connect peers who are behind a NAT or firewall.

Peers connect using QUIC, with client authentication using ed25519. A QUIC stream is opened for each RPC request to a peer. There are two types of request:

- `ls` - for querying the shared file index
- `read` - for downloading a file, or part of a file.

## Roadmap

- [x] Handshaking / prove knowledge of 'topic'
- [ ] MQTT should not use `retain` but periodically publish messages
- [ ] Support multiple topics / dynamically joining or leaving a topic
- [x] Persistent cryptographic identities / derived peer names
- [ ] Query own files / represent self as a peer
- [ ] Attempt to reconnect to peers after loosing connection
- [x] Holepunching / peer discovery over internet
- [ ] Holepunching for symmetric nat using birthday paradox. [See Tailscale nat traversal article](https://tailscale.com/blog/how-nat-traversal-works)
- [x] Download files rather than just read them
- [ ] Handle recursive directory download request
- [ ] Queueing / resume downloads
- [ ] Web UI (first attempt is here - using Leptos: https://gitlab.com/pegpeg/harddrive-party-web-ui )

This is based on a previous NodeJS project - [pegpeg/hdp](https://gitlab.com/pegpeg/hdp) - but has many protocol-level changes.
