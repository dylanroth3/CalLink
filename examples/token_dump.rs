//! Maintenance helper: prints every tokenizer lookup in a fixed order, so two versions of
//! the token table can be compared with diff.
fn main() {
    println!("{}", callink::tokens::TokenTable::get().canonical_dump());
}
