//! OpenAPI doc drift check (ADR-049 amendment): the committed `docs/openapi.json`
//! must match the spec derived from the annotated handlers, so the browsable
//! documentation never rots. Regenerate it with the command in COMMANDS.md.

use backend::adapters::http::ApiDoc;
use utoipa::OpenApi;

const DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/openapi.json");

#[test]
fn committed_openapi_json_matches_the_code() {
    let generated = ApiDoc::openapi().to_pretty_json().unwrap();
    let committed = std::fs::read_to_string(DOC).expect("docs/openapi.json is committed");
    assert_eq!(
        generated.trim(),
        committed.trim(),
        "docs/openapi.json is stale — regenerate it (see COMMANDS.md: OpenAPI docs)"
    );
}
