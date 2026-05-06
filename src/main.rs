// Native binary entry — wasm uses #[wasm_bindgen(start)] from lib.rs.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    rustwebtest::native_main();
}

#[cfg(target_arch = "wasm32")]
fn main() {
    // No-op on wasm; wasm-bindgen calls wasm_main from lib.rs.
}
