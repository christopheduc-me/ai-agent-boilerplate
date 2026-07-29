//! Prints the public OpenAPI spec (ADR-049 amendment). Regenerate the committed
//! browsable doc with (from `backend/`):
//!     cargo run --example openapi > ../docs/openapi.json
use backend::adapters::http::ApiDoc;
use utoipa::OpenApi;

fn main() {
    println!("{}", ApiDoc::openapi().to_pretty_json().unwrap());
}
