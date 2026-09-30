// The `.template` suffix keeps these manifests from being picked up as real
// workspace members inside this repo; restore the real names after generation.
file::rename("Cargo.toml.template", "Cargo.toml");
file::rename("README.md.template", "README.md");
file::rename("fuzz/Cargo.toml.template", "fuzz/Cargo.toml");
