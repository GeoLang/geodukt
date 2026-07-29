//! geodukt CLI — declarative geospatial ETL pipeline tool.

use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use geodukt_core::manifest::Manifest;
use geodukt_core::pipeline::Pipeline;
use geodukt_io::docgen;
use geodukt_io::formats::{MultiFormatReader, MultiFormatWriter};
use geodukt_transforms::registry::default_registry;

#[derive(Parser)]
#[command(name = "geodukt", about = "Declarative geospatial ETL pipeline")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the pipeline defined in geodukt.toml
    Run {
        #[arg(short, long, default_value = "geodukt.toml")]
        manifest: PathBuf,
    },
    /// Validate the pipeline DAG without executing
    Validate {
        #[arg(short, long, default_value = "geodukt.toml")]
        manifest: PathBuf,
    },
    /// Show the execution order (DAG topological sort)
    Graph {
        #[arg(short, long, default_value = "geodukt.toml")]
        manifest: PathBuf,
    },
    /// Initialize a new pipeline project
    Init { name: String },
    /// Start the REST API server
    Serve {
        /// Address to bind to
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        bind: String,
    },
    /// Generate documentation from manifest
    Docs {
        #[arg(short, long, default_value = "geodukt.toml")]
        manifest: PathBuf,
        /// Output format: markdown or html
        #[arg(short, long, default_value = "markdown")]
        format: String,
        /// Output file (stdout if not specified)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Diff pipeline outputs between git commits
    Diff {
        #[arg(short, long, default_value = "geodukt.toml")]
        manifest: PathBuf,
        /// Git ref to compare against (default: HEAD~1)
        #[arg(long, default_value = "HEAD~1")]
        from: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Run { manifest } => cmd_run(&manifest),
        Command::Validate { manifest } => cmd_validate(&manifest),
        Command::Graph { manifest } => cmd_graph(&manifest),
        Command::Init { name } => cmd_init(&name),
        Command::Serve { bind } => cmd_serve(&bind).await,
        Command::Docs {
            manifest,
            format,
            output,
        } => cmd_docs(&manifest, &format, output.as_deref()),
        Command::Diff { manifest, from } => cmd_diff(&manifest, &from),
    }
}

fn load_manifest(path: &PathBuf) -> Manifest {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {e}", path.display());
        std::process::exit(1);
    });
    Manifest::from_toml(&content).unwrap_or_else(|e| {
        eprintln!("Error parsing manifest: {e}");
        std::process::exit(1);
    })
}

fn cmd_run(path: &PathBuf) {
    let manifest = load_manifest(path);
    let pipeline = Pipeline::new(manifest).unwrap_or_else(|e| {
        eprintln!("Pipeline error: {e}");
        std::process::exit(1);
    });

    let transforms = default_registry();
    let reader = MultiFormatReader;
    let writer = MultiFormatWriter;

    let report = pipeline
        .execute(&reader, &transforms, &writer)
        .unwrap_or_else(|failure| {
            for step in &failure.completed {
                eprintln!("  {} — {} features", step.name, step.feature_count);
            }
            eprintln!("Execution error: {}", failure.error());
            std::process::exit(1);
        });

    println!("Pipeline completed successfully:");
    for step in &report.steps {
        println!("  {} — {} features", step.name, step.feature_count);
    }
}

fn cmd_validate(path: &PathBuf) {
    let manifest = load_manifest(path);
    let pipeline = Pipeline::new(manifest).unwrap_or_else(|e| {
        eprintln!("Validation failed: {e}");
        std::process::exit(1);
    });

    let order = pipeline.validate().unwrap_or_else(|e| {
        eprintln!("Validation failed: {e}");
        std::process::exit(1);
    });

    println!("Pipeline is valid. Execution order:");
    for (i, name) in order.iter().enumerate() {
        println!("  {}. {name}", i + 1);
    }
}

fn cmd_graph(path: &PathBuf) {
    let manifest = load_manifest(path);
    let pipeline = Pipeline::new(manifest).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });

    let order = pipeline.validate().unwrap();
    println!("DAG execution order:");
    for (i, name) in order.iter().enumerate() {
        if i > 0 {
            println!("    ↓");
        }
        println!("  [{name}]");
    }
}

fn cmd_init(name: &str) {
    let dir = PathBuf::from(name);
    fs::create_dir_all(&dir).unwrap_or_else(|e| {
        eprintln!("Error creating directory: {e}");
        std::process::exit(1);
    });
    fs::create_dir_all(dir.join("data")).unwrap();
    fs::create_dir_all(dir.join("output")).unwrap();

    let manifest = format!(
        r#"[project]
name = "{name}"
version = "0.1.0"

[[source]]
name = "input"
format = "geojson"
path = "data/input.geojson"

[[transform]]
name = "processed"
input = "input"
operation = "centroid"

[[sink]]
name = "output"
input = "processed"
format = "geojson"
path = "output/result.geojson"
"#
    );

    fs::write(dir.join("geodukt.toml"), manifest).unwrap();
    println!("Initialized new geodukt project in {name}/");
}

async fn cmd_serve(bind: &str) {
    println!("Starting geodukt server on {bind}");
    geodukt_server::serve(bind).await.unwrap_or_else(|e| {
        eprintln!("Server error: {e}");
        std::process::exit(1);
    });
}

fn cmd_docs(path: &PathBuf, format: &str, output: Option<&std::path::Path>) {
    let manifest = load_manifest(path);
    let content = match format {
        "html" => docgen::generate_html(&manifest),
        _ => docgen::generate_markdown(&manifest),
    };

    if let Some(out) = output {
        fs::write(out, &content).unwrap_or_else(|e| {
            eprintln!("Error writing docs: {e}");
            std::process::exit(1);
        });
        println!("Documentation written to {}", out.display());
    } else {
        print!("{content}");
    }
}

fn cmd_diff(path: &PathBuf, from_ref: &str) {
    let manifest = load_manifest(path);
    println!("Comparing pipeline outputs against {from_ref}...");

    // Get the manifest at the old ref
    let path_str = path.to_string_lossy();
    let old_output = std::process::Command::new("git")
        .args(["show", &format!("{from_ref}:{path_str}")])
        .output();

    match old_output {
        Ok(output) if output.status.success() => {
            let old_content = String::from_utf8_lossy(&output.stdout);
            match Manifest::from_toml(&old_content) {
                Ok(old_manifest) => {
                    // Compare sources
                    let new_sources: Vec<&str> =
                        manifest.source.iter().map(|s| s.name.as_str()).collect();
                    let old_sources: Vec<&str> = old_manifest
                        .source
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect();

                    for s in &new_sources {
                        if !old_sources.contains(s) {
                            println!("  + source: {s}");
                        }
                    }
                    for s in &old_sources {
                        if !new_sources.contains(s) {
                            println!("  - source: {s}");
                        }
                    }

                    // Compare transforms
                    let new_transforms: Vec<&str> =
                        manifest.transform.iter().map(|t| t.name.as_str()).collect();
                    let old_transforms: Vec<&str> = old_manifest
                        .transform
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect();

                    for t in &new_transforms {
                        if !old_transforms.contains(t) {
                            println!("  + transform: {t}");
                        }
                    }
                    for t in &old_transforms {
                        if !new_transforms.contains(t) {
                            println!("  - transform: {t}");
                        }
                    }

                    // Compare sinks
                    let new_sinks: Vec<&str> =
                        manifest.sink.iter().map(|s| s.name.as_str()).collect();
                    let old_sinks: Vec<&str> =
                        old_manifest.sink.iter().map(|s| s.name.as_str()).collect();

                    for s in &new_sinks {
                        if !old_sinks.contains(s) {
                            println!("  + sink: {s}");
                        }
                    }
                    for s in &old_sinks {
                        if !new_sinks.contains(s) {
                            println!("  - sink: {s}");
                        }
                    }

                    if new_sources == old_sources
                        && new_transforms == old_transforms
                        && new_sinks == old_sinks
                    {
                        println!("  No structural changes detected.");
                    }
                }
                Err(e) => {
                    eprintln!("Could not parse old manifest: {e}");
                }
            }
        }
        _ => {
            println!(
                "  No previous version found at {from_ref}. This appears to be a new pipeline."
            );
        }
    }
}
