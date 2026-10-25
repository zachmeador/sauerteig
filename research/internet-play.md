# Internet play options

How friends in different homes can join a world hosted on one friend's computer,
browser players included. Reference for [shared-sandbox](../docs/tracking/shared-sandbox.md)
stage 4. A research agent compiled this on 2026-10-25; the measurements came from
throwaway test crates outside the repository.

## Key facts

- **Today's snapshots are too big to fan out from a home connection.**
  - A 16-player JSON snapshot is 3,937 bytes.
  - At 20 Hz, sending it to 15 remote players costs the host 9.4 Mbit/s of upload.
  - The browser bundle is 18.9 MB.
  - So the host's connection should neither fan out snapshots nor serve the page.
- **The browser page must come from HTTPS** (tested in Chromium 152).
  - A page on `http://<LAN IP>` is not a secure context, so `navigator.gpu` and
    `WebTransport` are undefined.
  - An HTTPS page cannot open `ws://` to a remote host.
- **WebTransport is in all major browsers as of 2026:** Chrome 97, Firefox 114,
  Safari 26.4.
  - `serverCertificateHashes` lets a page trust a self-signed server
    certificate: Chrome 100, Firefox 125, Safari 26.4.
  - Certificates must be X.509v3, valid at most two weeks, and ECDSA P-256.
  - Chromium 152 accepted 13- and 14-day certificates and rejected a 15-day,
    wrong-hash, or unpinned one.
  - The maximum datagram observed was 1,024 bytes with wtransport defaults.
- **Only WebRTC can get a browser through NAT directly.** Browsers cannot listen or
  hole-punch otherwise. A 4.4-million-attempt libp2p study found hole punching
  succeeds about 70% of the time.

## Options

1. **Project-run relay (WSS now, WebTransport later).**
   - **How it works:** the host app dials out to a relay and shows a link. Browser
     friends click it; native friends paste the `wss://` address.
   - **Hosting:** the page is served from the relay's HTTPS origin or a CDN.
   - **Trade-offs:**
     - The extra hop costs an estimated 10–40 ms of round trip (unmeasured).
     - It works through any NAT, including carrier-grade NAT.
   - **Cost:** about $4–6 a month for a small VPS, plus a domain.
     - Cloudflare Durable Objects are cheaper to start, but have a soft cap of
       1,000 requests per second per object, while this game sends about 960 input
       messages per second.
   - **Security:**
     - The relay sees plaintext.
     - The link is the credential.
     - The host's IP stays hidden, and no inbound port is needed.
   - **Effort:** we write and operate it; it is small.
2. **WebRTC data channels.**
   - **Strengths:**
     - Direct UDP, so the best latency.
     - About 70–90% of connections go direct; the rest fall back to TURN
       (Cloudflare charges $0.05/GB after 1,000 GB a month).
     - End-to-end DTLS encryption.
   - **Requirements:** signaling plus free STUN. The page can come from any HTTPS
     static host.
   - **Downsides:**
     - The most code to write.
     - Rust stacks are immature: str0m 0.24, webrtc-rs 0.21, and matchbox_socket
       0.14.
3. **Direct WebTransport with a pinned certificate.**
   - **How it works:** the host app makes a short-lived certificate and binds a
     UDP port. The host needs inbound UDP via UPnP/PCP, IPv6, or a manually
     forwarded port.
   - **Hosting:** the page must still come from a public HTTPS host.
   - **Trade-offs:**
     - It has the best latency, but only reachable hosts can use it.
     - Some networks block QUIC.
     - It exposes the host's IP and needs join tokens.
   - **Crate maturity:**
     - Native crates (quinn, wtransport) work.
     - The browser crates (web-transport-wasm, xwt-web) need newer
       wasm-bindgen/web-sys than the repository pins (`=0.2.100` / `=0.3.77`).
4. **Tunnels.**
   - **Cloudflare quick tunnel:** the app can spawn `cloudflared` with no account,
     but gets a new hostname every run. Cloudflare documents quick tunnels as for
     testing only, with no uptime guarantee, and has no documented UDP support.
   - **Tailscale Funnel** is in beta.
   - **ngrok's free plan** allows 1 GB a month.
   - **What works today:** serving the page from a CDN with
     `?server=wss://<tunnel>`.
5. **Port mapping (UPnP/PCP) plus a public page.**
   - **Unreliable:** success rates are unknown, it is often disabled on routers,
     and it fails behind carrier-grade or double NAT.
   - **Hard to secure:**
     - WSS still needs a trusted certificate.
     - It exposes an internet-facing port, so tokens and rate limits are mandatory.
6. **A VPS running the world directly.**
   - **Setup:** the owner runs `sauerteig-server` with Caddy for automatic HTTPS.
     This works today.
   - **Trade-offs:**
     - It costs about $4–6.50 a month.
     - The world lives on the VPS instead of the friend's computer.
     - Nobody gets a host advantage.

## Recommendation

**Decision:** the maintainer ruled out paid services on 2026-10-25, so stage 4
starts with free options instead (a quick tunnel, then Tailscale). See
[shared-sandbox](../docs/tracking/shared-sandbox.md) stage 4. The relay design
below remains the reference if that changes.

**Recommended first version: a project relay on a small VPS.**

The host experience:
- The host clicks "Host online".
- The app dials out and shows a stable link.
- Friends click it in a browser or paste it into the native app.
- There is no router, certificate, or account step.

Design:
- **One relay binary**, built on axum and tungstenite (both already in use), behind
  Caddy.
- **The relay serves the browser build for each version,** so the page and the
  host's protocol always match.
- **The host proves a per-world key,** and the room identifier is its hash, so the
  relay cannot impersonate hosts.
- **One multiplexed connection from the host** carries open, data, close, and
  broadcast frames. Snapshots upload once (about 0.6 Mbit/s, not 9.4).
- **On the server side the relay is one more acceptor** that feeds the per-peer
  channels from shared-sandbox stage 1.
- **Safeguards:** join tokens, the 16-player cap, rate limits, and an Origin check.
- **Keep LAN mode and the custom server field.**

Build it after shared-sandbox stages 2 and 3. The relay hop shows up first as edit
latency, and a 45 MB JSON welcome takes 36 seconds per joiner at 10 Mbit/s. Measure
real relay round trips with the group before building anything direct.

## Long-term path to datagrams

1. **Now (no regret):** a link boundary with two lanes.
   - **Reliable, framed:** welcome, chunks, edits, and patches.
   - **Unreliable:** about 1 KB per message. Input (with the last few inputs
     repeated), snapshots, and shot effects go here.
   - Over WebSocket, the unreliable lane behaves as "latest wins", which matches
     how snapshots already work.
   - Snapshots must be quantized to fit about 1 KB (JSON is 3.9 KB today).
2. **Combat stage:** WebTransport on the same VPS.
   - Use an ordinary Let's Encrypt certificate, plus a QUIC leg from the host.
   - The `web-transport` crate offers one API for native and browser; adopting it
     means unpinning wasm-bindgen and web-sys.
   - Fall back to WebSocket for older Safari and networks that block UDP. `qmux`
     (IETF draft) offers the same API over WebSocket.
3. **Only if measurements demand it:** start relayed, then upgrade to a direct
   connection (as Tailscale does).
   - The host advertises a mapped UDP address and a certificate hash through the
     relay.
   - Joiners try direct WebTransport briefly, then stay relayed.
   - WebRTC is the last resort for hosts behind NAT.

## Unverified

- Safari 26.4's behavior with pinned certificates (sources only).
- Whether Firefox enforces the 14-day rule (only Chromium was tested).
- Actual relay round-trip times.
- How quick tunnels and Funnel behave with WebSockets, and Funnel's cap.
- Whether Workers/Durable Objects accept inbound WebTransport.
- UPnP/PCP success rates and how common carrier-grade NAT is.
- The web-transport and xwt crates (read, not built).
- WebRTC interop of str0m and webrtc-rs with browsers.

## Sources

MDN browser-compat-data (WebTransport); W3C WebTransport spec and issue 623; WebKit
blog 17862 and bug 300057; Mozilla bug 1873263; MDN Local Network Access;
arXiv 2510.27500 (libp2p hole punching); Tailscale "How NAT traversal works";
Cloudflare Realtime/TURN pricing, Durable Objects limits, Workers pricing, quick
tunnel docs, and network page; Tailscale Funnel docs; ngrok free-plan limits; Let's
Encrypt IP-address certificates (2025-07-01); libp2p AutoTLS; Hetzner and
DigitalOcean pricing; crates.io (25 Oct 2026) and READMEs of str0m, webrtc-rs,
wtransport, and web-transport; IETF draft-ietf-quic-qmux; iroh docs.
