//! Read-only qualification of an explicitly supplied JEX archive.
//! The staging parent receives an owned source spool only during this process;
//! it is removed before exit. No native library profile is created.

use std::{env, process::ExitCode};

use app_lite_core::{JexQualificationError, qualify_jex_archive};

fn main() -> ExitCode {
    let mut args = env::args_os();
    let _program = args.next();
    let (Some(archive), Some(parent), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: qualify_jex <explicit-archive.jex> <existing-staging-parent>");
        return ExitCode::from(2);
    };
    match qualify_jex_archive(archive, parent) {
        Ok(report) => {
            println!("semantic_scan_completed={}", report.semantic_scan_completed);
            println!("ready_for_current_stage={}", report.ready_for_current_stage);
            println!(
                "notes={} folders={} resource_metadata={} physical_resource_files={} tags={} note_tag_relations={}",
                report.counts.notes,
                report.counts.folders,
                report.counts.resource_metadata,
                report.counts.physical_resource_files,
                report.counts.tags,
                report.counts.note_tag_relations
            );
            for (item_type, count) in &report.metadata_item_type_counts {
                println!("metadata_item_type={item_type} count={count}");
            }
            for (mime, count) in &report.resource_mime_counts {
                println!("resource_mime={mime} count={count}");
            }
            for category in report.categories {
                println!(
                    "category={:?} items={} findings={} samples={}",
                    category.kind,
                    category.item_count,
                    category.finding_count,
                    category.samples.len()
                );
                for sample in category.samples {
                    println!(
                        "  source_id={} source_path={} field={}",
                        sample.source_id,
                        sample.source_path,
                        sample.field.as_deref().unwrap_or("-")
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            // Do not echo archive paths or parser-provided text on this CLI.
            let category = match error {
                JexQualificationError::Prepare(_) => "source preparation",
                JexQualificationError::Cancelled => "cancelled",
                JexQualificationError::Source(_) => "verified source",
            };
            eprintln!("qualification failed: {category}");
            ExitCode::from(1)
        }
    }
}
