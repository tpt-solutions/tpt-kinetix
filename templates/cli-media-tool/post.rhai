// The `.template` suffix keeps the manifest from being treated as a workspace
// member of the template repo; restore the real name after generation.
file::rename("Cargo.toml.template", "Cargo.toml");
