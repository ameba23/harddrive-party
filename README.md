
## harddrive-party

Allows two or more peers to share files. Peers can choose directories to share, and a 'topic' name to meet at.

Currently **work-in-progress**.

## Design goals

- Minimal setup - don't need to wait long for files to index
- Practical for transferring large media collections
- Remote control via ws / http interface. Can be run on an ARM device or NAS and controlled from another computer
- Minimal reliance on centralised infrastructure

## Protocol

Peers currently can only discover each other using mDNS - only peers on the same local network will be found.

Peers connect using QUIC. A QUIC stream is opened for each RPC request to a peer. There are two types of request.
- `ls` - for querying the shared file index
- `read` - for downloading a file, or part of a file.

## Roadmap

- [ ] Handshaking / prove knowledge of 'topic'
- [ ] Support multiple topics
- [ ] Persistent cryptographic identities / derived peer names
- [ ] Query own files / represent self as a peer
- [ ] Holepunching / peer discovery over internet
- [ ] Download files rather than just read them
- [ ] Handle recursive directory download request
- [ ] Queueing / resume downloads
- [ ] Web UI

This is based on a previous NodeJS project - [pegpeg/hdp](https://gitlab.com/pegpeg/hdp) - but has many protocol-level changes.
