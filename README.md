# Sauerteig

A multiplayer FPS and cooperative cube editor inspired by Quake, Sauerbraten,
and Tesseract. Press **E** to switch between fighting and building in the same
live arena. Written in Rust, with native desktop and WebGPU browser clients.

## Run locally

Install [Rust](https://rustup.rs/), then run these commands from the repository
root. Linux builds also need the [system libraries](docs/development.md#linux-dependencies).

Start the native app:

```sh
. ./scripts/env.sh
cargo run -p sauerteig-client
```

Choose **New world**, **Continue**, or **Join server**. Opening a local world
starts its server automatically. **Main menu** returns to the world picker while
the session keeps running; **Resume** returns to it. Choosing another world saves
and closes the previous local session before opening the next one.
Named worlds, player name, and the last server are remembered. Local hosting
accepts players on this computer; enable LAN hosting to let other computers join.

On macOS, `./scripts/build-macos-app.sh` creates `.local/Sauerteig.app`, including
the browser client. Double-click it to play. Existing world JSON files can be
copied into the world picker through **Import an existing world**.

### Browser

Install the browser build tools once:

```sh
. ./scripts/env.sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.100 --locked
```

Build the client and run a standalone server:

```sh
./scripts/build-web.sh
cargo run -p sauerteig-server
```

Open **http://localhost:3000/?name=Player** in a browser with WebGPU enabled.
Browser and native players share the same room. Hosting away from localhost
requires HTTPS and secure WebSockets. See [development and hosting](docs/development.md).

The native app's Session panel also provides the browser address for its hosted
world. The browser can join servers; creating and hosting worlds uses the native
app or standalone server.

## Playing and editing

WASD moves, the mouse aims, Space jumps, and left click fires. In edit mode,
left-drag selects geometry and the wheel extrudes or carves it. Hold **G** while
scrolling to change the grid size. **T** opens materials and props, **B** opens
blueprints, and **F2** opens settings, including scroll inversion.

- [Editor controls, selections, and blueprints](docs/editor.md)
- [Importing Sauerbraten and older Tesseract maps](crates/importer/README.md)
- [Development, hosting, and performance checks](docs/development.md)
- [Project tracking: complete, active, and unfinished work](docs/tracking/README.md)
- [Geometry and multiplayer invariants](crates/core/CONTRACT.md)
- [Materials](assets/materials/README.md), [props](assets/props/README.md), and [art direction](research/assets.md)

Sauerteig is a playable prototype. **Legacy import is partial:** supported cube
geometry, water/glass volumes, and light/spawn positions can be loaded, but original textures, models,
light properties, and gameplay entities are not restored. A map opening
successfully is not evidence of visual or gameplay fidelity. See the
[importer's limitations](crates/importer/README.md#appearance-and-entities).

Rooms support up to 16 players and the editor
grid reaches 1/8 metre. Imported worlds use compact geometry and their own bounds,
up to 8,192 metres per axis. Props and lights support editing, undo, and blueprints;
props currently have no gameplay collision.
Public-server authentication and moderation are not implemented. Ordinary-laptop
performance and combat over internet connections still need broader playtesting.

## License

Original code and documentation are [MIT licensed](LICENSE). The included
materials and prop models are CC0; their sources and modifications are recorded
in the asset manifests. See [third-party notices](THIRD_PARTY_NOTICES.md).
