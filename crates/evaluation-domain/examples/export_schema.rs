//! Writes the checked-in JSON Schema artifacts derived from Rust domain types.

use std::error::Error;
use std::path::PathBuf;

use evaluation_domain::{evaluation_spec_schema, goal_review_schema};

fn main() -> Result<(), Box<dyn Error>> {
    let output_directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: export_schema <output-directory>")?;
    std::fs::create_dir_all(&output_directory)?;
    write_schema(
        output_directory.join("evaluation-spec.v1alpha1.schema.json"),
        &evaluation_spec_schema()?,
    )?;
    write_schema(
        output_directory.join("goal-review.v1.schema.json"),
        &goal_review_schema()?,
    )?;
    Ok(())
}

fn write_schema(path: PathBuf, schema: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    let mut output = serde_json::to_string_pretty(schema)?;
    output.push('\n');
    std::fs::write(path, output)?;
    Ok(())
}
