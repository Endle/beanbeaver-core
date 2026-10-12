//! Text-line extraction, with separate row, quantity, pairing and reconciliation stages.
mod code_price_table;
mod engine;
mod pairing;
mod patterns;
mod quantity;
mod reconcile;
mod rows;
mod tokens;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use engine::extract_text_items;
