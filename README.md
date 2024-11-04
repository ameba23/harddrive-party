
![](./web-ui/public/img/hdd.png)

## harddrive-party

Allows two or more peers to share files. Peers can choose directories to share, and a 'topic' name to meet at.

## Features / design goals

- Local and remote download/upload queueing. 
- Minimal initial setup - don't need to wait long for shared files to index (no hashing).
- Udp hole-punching - with support for asymmetric to symmetric NAT connections using the birthday paradox. For an explanation see [Tailscale's NAT traversal article](https://tailscale.com/blog/how-nat-traversal-works).
- Practical for transferring large media collections.
- Remote control via websocket / HTTP interface. Can be run on a headless device or NAS and controlled from another computer.
- Minimal reliance on centralised infrastructure - servers are only used for peer discovery.
- Offline-first - peers can find each other on local network using mDNS.
- Support slow / intermittent connections - downloads continue from where they left off following dropped connection or restarting the process.
- Hackable open source protocol.

## Usage

`harddrive-party start --topic "place to meet" --share-dir ~/my-dir-to-share`

Decide on a 'topic' to meet on. This can be any string. You will be able to connect to anyone who 'joins' the same topic name. Choose a directory to share.

Open `http://localhost:3030` in your browser.

Once the remote peer(s) have connected you should be able to see their shared files in the 'Peers' tab.

Download a file or directory by clicking the download button next to it. You can see the status of downloads and view downloaded files in the 'Transfers' tab. 

## Protocol

### Peer discovery

Peers discovery each other by connecting to a public [MQTT](https://en.wikipedia.org/wiki/MQTT) server and announcing their IP address encrypted with the hash of the topic name. On seeing another peer's announce message, we attempt to decrypt it with the topics we are connected to.

[Multicast DNS](https://en.wikipedia.org/wiki/Multicast_DNS) is also used to find peers connected to the same local network.

Since announcement messages are encrypted it is only possible to find peers who know the topic name. Udp hole-punching is used to connect peers who are behind a NAT or firewall.

By default the public MQTT server `broker.hivemq.com:1883` is used. This project has no affiliation with hivemq. If you prefer to use a different server or host your own server, you can pass the `--mqtt-server` command line option when starting harddrive-party. MQTT 3.1.1 is used, and the client is tested against [mqttest](https://github.com/vincentdephily/mqttest).

### Transport

Peers connect to each other using [Quic](https://en.wikipedia.org/wiki/QUIC), with client authentication using ed25519. A Quic stream is opened for each RPC request to a peer. There are two types of request message:

- `ls` - for querying the shared file index (with a sub-path, or search term).
- `read` - for downloading a file, or portion of a file. 

[Wire messages](./shared/src/wire_messages.rs) are serialized with [bincode](https://docs.rs/bincode).

### Shared files

To speed up file index queries, shared directory is indexed using a key-value store ([sled](https://docs.rs/sled)). The only metadata stored is the filenames and their size.

A 'wishlist' of requested files is also stored in the database so that if a connection is lost, the files will be re-requested next time you connect to that peer.

### Peer names

There are no usernames - peers are represented by an adjective and type of animal which is derived from their public authentication key. For example: 'PersianChinchilla'.

## Logging

You can switch on logging by setting the environment variable `RUST_LOG=harddrive_party=debug` or by starting with the `--verbose` command line option.

## Web user interface

There is a work-in-progress web-based front end build with [Leptos](https://docs.rs/leptos), served by default to `http://localhost:3030`. Source code in [`./web-ui`](./web-ui)

## Project status

Currently pre-alpha - expect bugs and breaking changes.

This is based on a previous NodeJS project - [pegpeg/hdp](https://gitlab.com/pegpeg/hdp) - but has many protocol-level changes.
