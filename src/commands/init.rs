use std::path::Path;

use crate::output::{self, Format};
use crate::project;

pub fn run(project_root: &Path, format: Format, quiet: bool) -> anyhow::Result<()> {
    let (project_id, was_created) = project::ensure_gcode_json(project_root)?;

    let status = if was_created {
        "initialized"
    } else if project_root.join(".gobby").join("project.json").exists() {
        "gobby"
    } else {
        "existing"
    };

    match format {
        Format::Json => output::print_json(&serde_json::json!({
            "project_id": project_id,
            "project_root": project_root.to_string_lossy(),
            "status": status,
        })),
        Format::Text => {
            if !quiet {
                match status {
                    "initialized" => {
                        eprintln!(
                            "Initialized project at {}\nProject ID: {}",
                            project_root.display(),
                            project_id
                        );
                    }
                    "gobby" => {
                        eprintln!(
                            "Using gobby project: {} ({})",
                            project_id,
                            project_root.display()
                        );
                    }
                    _ => {
                        eprintln!(
                            "Already initialized: {} ({})",
                            project_id,
                            project_root.display()
                        );
                    }
                }
            }
            Ok(())
        }
    }
}
