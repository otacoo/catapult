# Agent instructions

- After completing a set of working changes, commit them with a short, imperative message (match the existing terse style, e.g. "QuickBench", "Fix llama-bench orphans and bench state across tab switches"). Group related changes into separate commits when they are logically distinct.
- Run `npx tsc --noEmit` for frontend changes and `cargo test --manifest-path src-tauri/Cargo.toml` for Rust changes before committing.
