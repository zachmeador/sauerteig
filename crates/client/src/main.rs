fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    sauerteig_client::run();
}
