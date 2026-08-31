# Rust style guidelines (berger)

Goal: fast, easy PR reviews. Explicit and boring beats clever.

## Rules

- **Explicit over clever** — prefer plain `for`/`match`/`if` over long iterator chains (`.filter().map().fold()...`) when a loop reads faster.
- **No unnecessary generics** — write the concrete type first; generalize only when a second real caller needs it.
- **No macros** unless they materially simplify the design (e.g. remove real duplication). Prefer plain functions.
- **Named types over tuples** — `struct SessionId(String)` / a named struct beats `(String, String)`.
- **Enums for meaningful state** — model state machines and variants as `enum`, not bools/strings/magic numbers.
- **Small functions** — one responsibility, easy to name, easy to review in isolation.
- **No premature abstractions** — don't add traits/layers until a second concrete need exists.
- **Straightforward ownership** — prefer owned data / cloning over lifetime gymnastics unless perf actually requires it.
- **No `unsafe`** unless absolutely necessary; justify with a comment when used.
- **Readable `match` over combinators** — prefer `match`/`if let` over `.map_or()`/`.and_then()` chains when it improves clarity.

## Before opening a PR

```bash
cargo fmt
cargo clippy
cargo check
cargo test
```
