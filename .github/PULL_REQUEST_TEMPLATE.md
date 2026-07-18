## What does this PR do?

<!-- A short description of the change and the motivation. -->

## Related issues

<!-- e.g. Closes #123 -->

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactor / internal change
- [ ] Documentation
- [ ] CI / tooling

## Checklist

See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for details. All boxes should be
checked (or explained) before requesting review:

- [ ] `cargo fmt --all --check` passes (`just fmt-check`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes (`just clippy`)
- [ ] `cargo test --workspace` passes (`just test`)
- [ ] Doctests pass (`just test-doc`)
- [ ] `cargo deny check` passes (`just deny`) if dependencies changed
- [ ] New/changed public APIs have doc comments (and doc examples where useful)
- [ ] `todo.md` and/or crate README updated if this changes project status
- [ ] For parser changes: a fuzz target and/or regression corpus entry was considered

## Notes for reviewers

<!-- Anything specific you want feedback on. -->
