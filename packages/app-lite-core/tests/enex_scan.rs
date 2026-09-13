use std::io::Write;

use app_lite_core::{EnexScanError, scan_enex_file};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

fn scan_fixture(xml: &str) -> Result<app_lite_core::EnexScanReport, EnexScanError> {
    let mut fixture = NamedTempFile::new().unwrap();
    fixture.write_all(xml.as_bytes()).unwrap();
    scan_enex_file(fixture.path())
}

#[test]
fn scans_chinese_rich_enml_and_keeps_every_resource_occurrence_for_later_staging() {
    // Mutation caught: dropping repeated tags/resources, treating MD5 as SHA-256,
    // or failing to record nested en-media references makes this golden report differ.
    let image = b"image bytes";
    let pdf = b"%PDF-not-a-real-document";
    let image_md5 = "bebb32c1d5592c44df47d1826cacc09b";
    let xml = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/enex/evernote-export3.dtd">
<en-export><note><title>中文清单</title><created>20260913T010203Z</created><updated>20260913T040506Z</updated><tag>收件箱</tag><tag>收件箱</tag><content><![CDATA[<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd"><en-note><div><i><en-todo checked="true"/><en-media hash="{image_md5}" type="image/png"/></i></div><table><tr><td>不应静默压平</td></tr></table></en-note>]]></content><resource><data encoding="base64">aW1hZ2UgYnl0ZXM=</data><mime>image/png</mime><resource-attributes><file-name>图.png</file-name></resource-attributes></resource><resource><data encoding="base64">JVBERi1ub3QtYS1yZWFsLWRvY3VtZW50</data><mime>application/pdf</mime><resource-attributes><file-name>未引用.pdf</file-name></resource-attributes></resource></note></en-export>"##
    );

    let report = scan_fixture(&xml).unwrap();

    assert_eq!(report.counts.notes, 1);
    assert_eq!(report.counts.resource_occurrences, 2);
    let note = &report.notes[0];
    assert_eq!(note.ordinal, 1);
    assert_eq!(note.title, "中文清单");
    assert_eq!(note.created_raw, "20260913T010203Z");
    assert_eq!(note.updated_raw, "20260913T040506Z");
    assert_eq!(note.tags, ["收件箱", "收件箱"]);
    assert_eq!(note.media_references.len(), 1);
    assert_eq!(note.media_references[0].hash_md5, image_md5);
    assert_eq!(note.media_references[0].mime, "image/png");
    assert!(
        note.unsupported_fidelity_constructs
            .contains(&"table".to_owned())
    );
    assert_eq!(report.resources[0].md5, image_md5);
    assert_eq!(
        report.resources[0].sha256,
        format!("{:x}", Sha256::digest(image))
    );
    assert_eq!(report.resources[0].filename, "图.png");
    assert_eq!(
        report.resources[1].sha256,
        format!("{:x}", Sha256::digest(pdf))
    );
    assert_eq!(report.unreferenced_resource_ordinals, vec![2]);
    assert!(report.unresolved_media_references.is_empty());
}

#[test]
fn reports_duplicate_and_missing_en_media_hashes_without_dropping_the_real_attachment() {
    // Mutation caught: deduplicating occurrence records or conflating an orphan
    // en-media reference with an unreferenced real attachment.
    let xml = r#"<en-export><note><title>refs</title><content><![CDATA[<en-note><en-media hash="900150983cd24fb0d6963f7d28e17f72" type="image/png"/><en-media hash="00000000000000000000000000000000" type="image/png"/></en-note>]]></content><resource><data encoding="base64">YWJj</data><mime>application/octet-stream</mime><resource-attributes><file-name>actually.png</file-name></resource-attributes></resource><resource><data encoding="base64">YWJj</data><mime>image/png</mime></resource></note></en-export>"#;

    let report = scan_fixture(xml).unwrap();

    assert_eq!(report.resources.len(), 2);
    assert_eq!(
        report.duplicate_resource_md5s,
        vec!["900150983cd24fb0d6963f7d28e17f72"]
    );
    assert_eq!(report.unresolved_media_references.len(), 1);
    assert_eq!(
        report.unresolved_media_references[0].hash_md5,
        "00000000000000000000000000000000"
    );
    assert_eq!(report.mime_mismatches.len(), 1);
    assert_eq!(
        report.mime_mismatches[0].declared_mime,
        "application/octet-stream"
    );
    assert_eq!(report.mime_mismatches[0].referenced_mime, "image/png");
    assert!(report.unreferenced_resource_ordinals.is_empty());
}

#[test]
fn rejects_invalid_base64_and_malformed_xml_instead_of_returning_partial_evidence() {
    // Mutation caught: accepting corrupt/non-base64 bytes or silently recovering broken nesting.
    let corrupt = r#"<en-export><note><title>x</title><resource><data encoding="base64">!!!!</data></resource></note></en-export>"#;
    assert!(matches!(
        scan_fixture(corrupt),
        Err(EnexScanError::InvalidBase64 { .. })
    ));

    let malformed = r#"<en-export><note><title>x</note></en-export>"#;
    assert!(matches!(
        scan_fixture(malformed),
        Err(EnexScanError::MalformedXml(_))
    ));

    let non_base64 = r#"<en-export><note><title>x</title><resource><data encoding="hex">616263</data></resource></note></en-export>"#;
    assert!(matches!(
        scan_fixture(non_base64),
        Err(EnexScanError::UnsupportedDataEncoding { .. })
    ));

    let entity = r#"<!DOCTYPE en-export [<!ENTITY boom "x">]><en-export><note><title>&boom;</title></note></en-export>"#;
    assert!(matches!(
        scan_fixture(entity),
        Err(EnexScanError::UnsafeXml(_))
    ));
}

#[test]
fn streams_a_resource_over_20_mib_and_reports_decoded_digests() {
    // Mutation caught: collecting the whole base64 field or hashing encoded
    // rather than decoded bytes cannot satisfy this large-resource contract.
    let mut fixture = NamedTempFile::new().unwrap();
    fixture
        .write_all(b"<en-export><note><title>large</title><resource><data encoding=\"base64\">")
        .unwrap();
    let chunk = b"QUFB".repeat(16 * 1024);
    for _ in 0..448 {
        fixture.write_all(&chunk).unwrap();
    }
    fixture
        .write_all(b"</data></resource></note></en-export>")
        .unwrap();

    let report = scan_enex_file(fixture.path()).unwrap();
    let resource = &report.resources[0];
    assert_eq!(resource.byte_count, 22_020_096);
    assert_eq!(
        resource.md5,
        format!("{:x}", Md5::digest(vec![b'A'; 22_020_096]))
    );
    assert_eq!(
        resource.sha256,
        format!("{:x}", Sha256::digest(vec![b'A'; 22_020_096]))
    );
}

#[test]
fn accepts_a_128_kib_resource_without_using_the_metadata_limit() {
    // Mutation caught: routing a valid data event through the 16 KiB metadata cap.
    let payload = vec![b'x'; 128 * 1024];
    let xml = format!(
        "<en-export><note><title>large-but-bounded</title><resource><data>{}</data><mime>application/octet-stream</mime></resource></note></en-export>",
        STANDARD.encode(&payload)
    );

    let report = scan_fixture(&xml).unwrap();

    assert_eq!(report.resources[0].byte_count, payload.len());
}

#[test]
fn never_matches_a_note_media_reference_to_another_notes_same_md5_attachment() {
    // Mutation caught: global MD5 correlation that hides a missing attachment
    // in note A because note B happens to carry identical bytes.
    let hash = "900150983cd24fb0d6963f7d28e17f72";
    let xml = format!(
        r#"<en-export><note><title>A</title><content><![CDATA[<en-note><en-media hash="{hash}" type="image/png"/></en-note>]]></content></note><note><title>B</title><resource><data>YWJj</data><mime>image/png</mime></resource></note></en-export>"#
    );

    let report = scan_fixture(&xml).unwrap();

    assert_eq!(report.unresolved_media_references.len(), 1);
    assert_eq!(report.unresolved_media_references[0].note_ordinal, 1);
    assert_eq!(report.unreferenced_resource_ordinals, vec![1]);
}

#[test]
fn accepts_literal_data_markup_inside_enml_cdata_and_long_legal_tag_attributes() {
    // Mutation caught: raw preflight mistaking ENML CDATA for outer resource
    // data or enforcing an arbitrary 256-byte XML-tag maximum.
    let long_attribute = "a".repeat(300);
    let xml = format!(
        r#"<en-export><note><title>x</title><content><![CDATA[<en-note><div data-long="{long_attribute}">literal <data>not a resource</data></div></en-note>]]></content></note></en-export>"#
    );

    let report = scan_fixture(&xml).unwrap();

    assert_eq!(report.counts.notes, 1);
    assert!(report.resources.is_empty());
}

#[test]
fn rejects_malformed_enml_and_stops_on_an_oversized_non_data_cdata_field() {
    // Mutation caught: accepting malformed ENML nesting or handing an
    // unbounded non-data CDATA event to quick-xml.
    let malformed_enml = r#"<en-export><note><title>x</title><content><![CDATA[<en-note><div></en-note>]]></content></note></en-export>"#;
    assert!(matches!(
        scan_fixture(malformed_enml),
        Err(EnexScanError::MalformedXml(_))
    ));

    let oversized_title = format!(
        "<en-export><note><title><![CDATA[{}]]></title></note></en-export>",
        "x".repeat(17 * 1024)
    );
    assert!(matches!(
        scan_fixture(&oversized_title),
        Err(EnexScanError::FieldTooLarge { .. })
    ));
}

#[test]
fn rejects_oversized_xml_metadata_and_duplicate_fields() {
    // Mutation caught: a callback allocating an unchecked attribute, comment,
    // PI or DTD body before validation, or resetting note field state at resource end.
    let long = "x".repeat(17 * 1024);
    for xml in [
        format!("<en-export x=\"{long}\"/>"),
        format!("<!--{long}--><en-export/>"),
        format!("<?probe {long}?><en-export/>"),
        format!("<!DOCTYPE en-export SYSTEM \"{long}\"><en-export/>"),
    ] {
        assert!(
            matches!(scan_fixture(&xml), Err(EnexScanError::FieldTooLarge { .. })),
            "{xml:.80}"
        );
    }
    let duplicate = "<en-export><note><title>a</title><resource><data>YQ==</data></resource><title>b</title></note></en-export>";
    assert!(matches!(
        scan_fixture(duplicate),
        Err(EnexScanError::Structure(_))
    ));
}

#[test]
fn rejects_bad_padding_and_accepts_whitespace_at_chunk_boundary() {
    // Mutation caught: decoding callbacks independently or accepting data after padding.
    let base = "QUFB".repeat(16 * 1024);
    let xml = format!(
        "<en-export><note><resource><data>{base}\nYg==</data></resource></note></en-export>"
    );
    let report = scan_fixture(&xml).unwrap();
    assert_eq!(report.resources[0].byte_count, 49_153);
    let corrupt = "<en-export><note><resource><data>Yg==YQ==</data></resource></note></en-export>";
    assert!(matches!(
        scan_fixture(corrupt),
        Err(EnexScanError::InvalidBase64 { .. })
    ));
}

#[test]
fn rejects_unbounded_repeated_attributes_and_tags() {
    // Mutation caught: individually bounded values still permit aggregate
    // metadata growth without a per-element/per-note count limit.
    let attrs = (0..300).map(|i| format!(" x{i}=\"y\"")).collect::<String>();
    let xml = format!("<en-export{attrs}/>");
    assert!(matches!(
        scan_fixture(&xml),
        Err(EnexScanError::TooManyEntities)
    ));
    let tags = "<tag>x</tag>".repeat(1100);
    let xml = format!("<en-export><note>{tags}</note></en-export>");
    assert!(matches!(
        scan_fixture(&xml),
        Err(EnexScanError::TooManyEntities)
    ));
}

#[test]
fn rejects_oversized_xml_names_before_owned_copy() {
    // Mutation caught: a syntactically valid but oversized name entering an owned String.
    let name = "n".repeat(300);
    for xml in [
        format!("<en-export><{name}/></en-export>"),
        format!("<en-export {name}=\"x\"/>"),
        format!("<?{name} content?><en-export/>"),
    ] {
        assert!(matches!(
            scan_fixture(&xml),
            Err(EnexScanError::FieldTooLarge { .. })
        ));
    }
    let huge = "n".repeat(17 * 1024);
    for xml in [
        format!("<en-export><{huge}/></en-export>"),
        format!("<en-export {huge}=\"x\"/>"),
        format!("<?{huge} content?><en-export/>"),
    ] {
        assert!(scan_fixture(&xml).is_err());
    }
}

#[test]
fn enml_requires_one_en_note_root_and_no_custom_entities() {
    // Mutation caught: a well-formed fragment or unexpanded entity appearing clean.
    for body in [
        "<div/>",
        "plain text",
        "<en-note>&boom;</en-note>",
        "<en-note/><en-note/>",
    ] {
        let xml =
            format!("<en-export><note><content><![CDATA[{body}]]></content></note></en-export>");
        assert!(scan_fixture(&xml).is_err(), "{body}");
    }
}

#[test]
fn nested_note_or_resource_under_attributes_fails_without_panic() {
    // Mutation caught: nested semantic names taking the active builder and
    // causing a later outer close to panic.
    for inner in ["<note/>", "<resource><data>YQ==</data></resource>"] {
        let xml = format!(
            "<en-export><note><note-attributes>{inner}</note-attributes></note></en-export>"
        );
        let outcome = std::panic::catch_unwind(|| scan_fixture(&xml));
        assert!(matches!(outcome, Ok(Err(EnexScanError::Structure(_)))));
    }
}

#[test]
fn aggregate_retained_metadata_budget_stops_repeated_large_tags() {
    // Mutation caught: per-field and per-note limits permitting unlimited
    // retained report growth across notes.
    let tag = format!("<tag>{}</tag>", "x".repeat(8 * 1024));
    let notes = (0..2)
        .map(|_| format!("<note>{}</note>", tag.repeat(600)))
        .collect::<String>();
    let xml = format!("<en-export>{notes}</en-export>");
    assert!(matches!(
        scan_fixture(&xml),
        Err(EnexScanError::ReportTooLarge { .. })
    ));
}

#[test]
fn aggregate_budget_also_bounds_derived_mime_mismatches() {
    // Mutation caught: bounded input references still expanding into an
    // unbounded report through reference-by-resource cross products.
    let media =
        "<en-media hash=\"900150983cd24fb0d6963f7d28e17f72\" type=\"image/png\"/>".repeat(400);
    let resources = "<resource><data>YWJj</data><mime>text/plain</mime></resource>".repeat(400);
    let xml = format!(
        "<en-export><note><content><![CDATA[<en-note>{media}</en-note>]]></content>{resources}</note></en-export>"
    );
    assert!(matches!(
        scan_fixture(&xml),
        Err(EnexScanError::ReportTooLarge { .. })
    ));
}
