//! Documentation generation — auto-generate lineage docs from manifests.

use geodukt_core::manifest::Manifest;

/// Render a path, naming the layer when the format has more than one.
fn location(path: &str, layer: Option<&str>) -> String {
    match layer {
        Some(layer) => format!("{path} (layer: {layer})"),
        None => path.to_string(),
    }
}

/// Generate Markdown documentation from a manifest.
pub fn generate_markdown(manifest: &Manifest) -> String {
    let mut doc = String::new();

    doc.push_str(&format!("# {}\n\n", manifest.project.name));
    doc.push_str(&format!("**Version:** {}\n\n", manifest.project.version));

    // Sources
    doc.push_str("## Sources\n\n");
    doc.push_str("| Name | Format | Path |\n");
    doc.push_str("|------|--------|------|\n");
    for src in &manifest.source {
        doc.push_str(&format!(
            "| {} | {} | {} |\n",
            src.name,
            src.format,
            location(&src.path, src.layer.as_deref())
        ));
    }
    doc.push('\n');

    // Transforms
    doc.push_str("## Transforms\n\n");
    doc.push_str("| Name | Input | Operation |\n");
    doc.push_str("|------|-------|----------|\n");
    for t in &manifest.transform {
        doc.push_str(&format!("| {} | {} | {} |\n", t.name, t.input, t.operation));
    }
    doc.push('\n');

    // Sinks
    doc.push_str("## Outputs\n\n");
    doc.push_str("| Name | Format | Path |\n");
    doc.push_str("|------|--------|------|\n");
    for s in &manifest.sink {
        doc.push_str(&format!(
            "| {} | {} | {} |\n",
            s.name,
            s.format,
            location(&s.path, s.layer.as_deref())
        ));
    }
    doc.push('\n');

    // Data flow (DAG)
    doc.push_str("## Data Flow\n\n");
    doc.push_str("```mermaid\ngraph LR\n");
    for src in &manifest.source {
        doc.push_str(&format!("    {}[\"📁 {}\"]\n", src.name, src.name));
    }
    for t in &manifest.transform {
        doc.push_str(&format!(
            "    {} --> |{}| {}\n",
            t.input, t.operation, t.name
        ));
    }
    for s in &manifest.sink {
        doc.push_str(&format!(
            "    {} --> {}[\"💾 {}\"]\n",
            s.input, s.name, s.name
        ));
    }
    doc.push_str("```\n");

    doc
}

/// Generate HTML documentation.
pub fn generate_html(manifest: &Manifest) -> String {
    let md = generate_markdown(manifest);
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<title>{} — Pipeline Documentation</title>
<style>
body {{ font-family: system-ui, sans-serif; max-width: 900px; margin: 2em auto; padding: 0 1em; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
th {{ background: #f5f5f5; }}
pre {{ background: #f8f8f8; padding: 1em; overflow-x: auto; }}
</style>
</head>
<body>
<pre>{}</pre>
</body>
</html>"#,
        manifest.project.name, md
    )
}
