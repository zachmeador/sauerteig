# Sauerteig

A multiplayer FPS and cooperative cube editor inspired by Quake, Sauerbraten,
and Tesseract. Press **E** to switch between fighting and building in the same
live arena. Written in Rust, with native desktop and WebGPU browser clients.

## Run locally

Install [Rust](https://rustup.rs/), then run these commands from the repository
root. Linux builds also need the [system libraries](docs/development.md#linux-dependencies).

Start the server:

```sh
. ./scripts/env.sh
cargo run -p sauerteig-server
```

In another terminal, start a client:

```sh
. ./scripts/env.sh
cargo run -p sauerteig-client -- --name Player
```

Additional clients join the same room. The default server is
`ws://127.0.0.1:3000/ws`; use the client's `--server` option to change it.
Worlds save automatically to `.local/world.json`. A new save starts with the
industrial arena; `--world PATH` selects another save.

### Browser

Install the browser build tools once:

```sh
. ./scripts/env.sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.100 --locked
```

Build the client, then start or restart the server:

```sh
./scripts/build-web.sh
cargo run -p sauerteig-server
```

Open **http://localhost:3000/?name=Player** in a browser with WebGPU enabled.
Browser and native players share the same room. Hosting away from localhost
requires HTTPS and secure WebSockets. See [development and hosting](docs/development.md).

On macOS, `./scripts/build-macos-app.sh` creates `.local/Sauerteig.app`.
The server runs separately.

## Playing and editing

WASD moves, the mouse aims, Space jumps, and left click fires. In edit mode,
left-drag selects geometry and the wheel extrudes or carves it. Hold **G** while
scrolling to change the grid size. **T** opens materials and props, **B** opens
blueprints, and **F2** opens settings, including scroll inversion.

- [Editor controls, selections, and blueprints](docs/editor.md)
- [Importing Sauerbraten and older Tesseract maps](crates/importer/README.md)
- [Development, hosting, and performance checks](docs/development.md)
- [Geometry and multiplayer invariants](crates/core/CONTRACT.md)
- [Materials](assets/materials/README.md), [props](assets/props/README.md), and [art direction](research/assets.md)

Sauerteig is under development. Rooms support up to 16 players, worlds are
128 × 48 × 128 metres, and the editor grid reaches 1/8 metre. Props currently
have no gameplay collision and are excluded from blueprints and geometry undo.
Public-server authentication and moderation are not implemented. Ordinary-laptop
performance and combat over internet connections still need broader playtesting.

## License

Original code and documentation are [MIT licensed](LICENSE). The included
materials and prop models are CC0; their sources and modifications are recorded
in the asset manifests. See [third-party notices](THIRD_PARTY_NOTICES.md).
