// Emit the macOS `-undefined dynamic_lookup` cdylib link arg when building as a
// Python extension module. pyo3 does not add this from its own build script, and
// maturin only adds it for wheel builds, so a plain `cargo build` on macOS would
// otherwise fail with unresolved `Py_*` symbols.
fn main() {
    if std::env::var_os("CARGO_FEATURE_EXTENSION_MODULE").is_some() {
        pyo3_build_config::add_extension_module_link_args();
    }
}
