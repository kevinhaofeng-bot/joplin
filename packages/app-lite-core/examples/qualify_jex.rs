//! Read-only qualification of an explicitly supplied JEX archive.
//! The staging parent receives an owned source spool only during this process;
//! it is removed before exit. No native library profile is created.

use std::{env, fmt::Write as _, process::ExitCode};

use app_lite_core::{JexQualificationError, JexQualificationReport, qualify_jex_archive};

fn render_report(report: &JexQualificationReport) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "semantic_scan_completed={}",
        report.semantic_scan_completed
    )
    .unwrap();
    writeln!(
        output,
        "ready_for_current_stage={}",
        report.ready_for_current_stage
    )
    .unwrap();
    writeln!(output,"notes={} folders={} resource_metadata={} physical_resource_files={} tags={} note_tag_relations={}",
        report.counts.notes,report.counts.folders,report.counts.resource_metadata,
        report.counts.physical_resource_files,report.counts.tags,report.counts.note_tag_relations).unwrap();
    for (item_type, count) in &report.metadata_item_type_counts {
        writeln!(output, "metadata_item_type={item_type} count={count}").unwrap();
    }
    for (item_type, count) in &report.unclassifiable_item_type_counts {
        writeln!(output, "unclassifiable_item_type={item_type} count={count}").unwrap();
    }
    for (mime, count) in &report.resource_mime_counts {
        writeln!(output, "resource_mime={mime} count={count}").unwrap();
    }
    for category in &report.categories {
        writeln!(
            output,
            "category={:?} items={} findings={} samples={}",
            category.kind,
            category.item_count,
            category.finding_count,
            category.samples.len()
        )
        .unwrap();
        for sample in &category.samples {
            writeln!(
                output,
                "  source_id={} source_path={} related_source_id={} field={}",
                sample.source_id,
                sample.source_path,
                sample.related_source_id.as_deref().unwrap_or("-"),
                sample.field.as_deref().unwrap_or("-")
            )
            .unwrap();
        }
    }
    output
}

fn main() -> ExitCode {
    let mut args = env::args_os();
    let _program = args.next();
    let (Some(archive), Some(parent), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: qualify_jex <explicit-archive.jex> <existing-staging-parent>");
        return ExitCode::from(2);
    };
    match qualify_jex_archive(archive, parent) {
        Ok(report) => {
            print!("{}", render_report(&report));
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

#[cfg(test)]
mod tests {
    use super::*;
    use app_lite_core::{
        JexQualificationBlockerKind, JexQualificationCategory, JexQualificationReport,
        JexQualificationSample, JexScanCounts,
    };

    #[test]
    fn cli_output_omits_blocker_reason_and_source_content() {
        let report = JexQualificationReport {
            counts: JexScanCounts::default(),
            metadata_item_type_counts: vec![(1, 1)],
            unclassifiable_item_type_counts: Vec::new(),
            resource_mime_counts: Vec::new(),
            categories: vec![JexQualificationCategory {
                kind: JexQualificationBlockerKind::PreflightIntegrity,
                item_count: 1,
                finding_count: 1,
                samples: vec![JexQualificationSample {
                    source_id: "11111111111111111111111111111111".into(),
                    source_path: "11111111111111111111111111111111.md".into(),
                    related_source_id: Some("22222222222222222222222222222222".into()),
                    field: Some("body".into()),
                    reason: "secret title and body".into(),
                }],
            }],
            fields: Vec::new(),
            semantic_scan_completed: true,
            ready_for_current_stage: false,
        };
        let output = render_report(&report);
        assert!(output.contains("related_source_id=22222222222222222222222222222222"));
        assert!(!output.contains("secret title and body"));
    }
}
