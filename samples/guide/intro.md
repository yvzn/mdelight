# Intro

Back to [README](../README.md). External: [Tauri](https://tauri.app).

## Sequence

```mermaid
sequenceDiagram
  User->>Editor: types
  Editor->>Rust: render_markdown
  Rust-->>Preview: html
```

## Code

```rust
fn main() { println!("needle in a haystack"); }
```
