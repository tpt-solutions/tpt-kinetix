# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> This crate is **never published** (`publish = false`). It is the shared implementation
> behind the `tpt-kinetix` CLI and the C, Python and Node.js bindings. Its changes are listed
> in the workspace [CHANGELOG.md](../CHANGELOG.md).

## [Unreleased]

### Added

- Library form of the container operations: probe JSON, remux, faststart, package and a run summary.
  The CLI and the language bindings call these functions instead of carrying their own copies.
