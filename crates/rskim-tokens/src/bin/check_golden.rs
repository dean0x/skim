#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Developer utility: print cl100k and o200k token counts for golden-vector verification.
//!
//! Regeneration script for AC3 and AC4 golden corpora.
//! Run with: `cargo run -p rskim-tokens --bin check_golden`
//! Pinned against tiktoken-rs 0.7.0 (workspace version).

fn main() {
    let cl100k = tiktoken_rs::cl100k_base().unwrap();
    let o200k = tiktoken_rs::o200k_base().unwrap();

    let inputs = &[
        "Hello, world!",
        "<|endoftext|>",
        "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}",
        "",
        "x",
        "日本語テスト",
        "The quick brown fox jumps over the lazy dog",
    ];

    for input in inputs {
        let cl = cl100k.encode_with_special_tokens(input).len();
        let o2 = o200k.encode_with_special_tokens(input).len();
        println!("cl={cl} o2={o2} {:?}", input);
    }
}
