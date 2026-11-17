//! The XTCE corpus runner (`just corpus`) on the examples, and on a corpus
//! made here that takes each of its paths.

use std::fs;
use std::path::{Path, PathBuf};

use mdl_fuzz::corpus;

const XTCE: &str = "http://www.omg.org/spec/XTCE/20180204";

const U8: &str = r#"<IntegerParameterType name="U8" signed="false"><IntegerDataEncoding sizeInBits="8"/></IntegerParameterType>"#;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn the_examples_build_and_decode() {
    let report = corpus::run(&root().join("examples/xtce"));
    assert_eq!(report.bugs(), 0, "{}", report.markdown());
    assert_eq!(report.built(), 2, "{}", report.markdown());
    assert!(report.decoded() > 0, "{}", report.markdown());
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// A document of one root SpaceSystem holding `body`.
fn document(name: &str, body: &str) -> String {
    format!(
        r#"<SpaceSystem name="{name}" xmlns="{XTCE}"><TelemetryMetaData>{body}</TelemetryMetaData></SpaceSystem>"#
    )
}

#[test]
fn each_kind_of_document_is_reported() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus");
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    // A packet whose parameter is in another document: refused alone for
    // the reference, then built with its directory, leaving out a document
    // that isn't XTCE.
    let types = document(
        "Types",
        &format!(
            r#"<ParameterTypeSet>{U8}</ParameterTypeSet><ParameterSet><Parameter name="Kind" parameterTypeRef="U8"/></ParameterSet>"#
        ),
    );
    let packets = document(
        "Sat",
        r#"<ContainerSet><SequenceContainer name="P"><EntryList><ParameterRefEntry parameterRef="/Types/Kind"/></EntryList></SequenceContainer></ContainerSet>"#,
    );
    write(&dir, "pair/types.xml", types.as_bytes());
    write(&dir, "pair/packets.xml", packets.as_bytes());
    write(
        &dir,
        "pair/legacy.xml",
        br#"<SpaceSystem name="Legacy" xmlns="http://example.com/legacy"/>"#,
    );
    // B at bit 4 overlaps A: it imports, and validation refuses it.
    let overlap = document(
        "Overlap",
        &format!(
            r#"<ParameterTypeSet>{U8}</ParameterTypeSet><ParameterSet><Parameter name="A" parameterTypeRef="U8"/><Parameter name="B" parameterTypeRef="U8"/></ParameterSet><ContainerSet><SequenceContainer name="P"><EntryList><ParameterRefEntry parameterRef="A"/><ParameterRefEntry parameterRef="B"><LocationInContainerInBits referenceLocation="containerStart"><FixedValue>4</FixedValue></LocationInContainerInBits></ParameterRefEntry></EntryList></SequenceContainer></ContainerSet>"#
        ),
    );
    write(&dir, "overlap.xml", overlap.as_bytes());
    write(&dir, "broken.xml", br#"<SpaceSystem name="Broken""#);
    // Latin-1, which the importer doesn't read.
    write(
        &dir,
        "latin1.xml",
        b"<SpaceSystem name=\"Caf\xe9\" xmlns=\"http://www.omg.org/spec/XTCE/20180204\"/>",
    );
    write(&dir, "notes.xml", b"<notes/>");
    write(&dir, ".hidden/stray.xml", packets.as_bytes());
    // A link back up, which would loop if the walk followed it.
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dir, dir.join("pair/up")).unwrap();

    let report = corpus::run(&dir);
    let markdown = report.markdown();
    assert_eq!(
        report.summary(),
        "6 imports (and a document retried with its directory): 2 built, 2 refused by the \
         importer, 1 refused by validation, 1 not text; 5 payloads decoded; 0 bugs in MDL",
        "{markdown}"
    );
    for row in [
        "| broken.xml | refused: xtce.syntax 1 |  |",
        "| latin1.xml | not UTF-8 |  |",
        "| overlap.xml | invalid: overlap 1 |  |",
        "| pair/legacy.xml | refused: xtce.document 1 |  |",
        "| pair/packets.xml | refused alone: xtce.reference 1; retried with its directory |  |",
        "| pair/ together: packets.xml, types.xml (left out: legacy.xml) | built: families 1, \
         sets 1, packets 1, commands 0; decodes: decoded 5 |  |",
        "| pair/types.xml | built: families 1, sets 1, packets 0, commands 0; decodes: none | \
         xtce.ignored 1 |",
    ] {
        assert!(markdown.contains(row), "{row}\n{markdown}");
    }
    assert!(
        markdown
            .contains("skipped for want of a SpaceSystem: 1. Unreadable files and directories: 0."),
        "{markdown}"
    );
    // The retried document's reference was resolved with its directory.
    assert!(!markdown.contains("| `xtce.reference` |"), "{markdown}");
    assert!(!markdown.contains("stray"), "{markdown}");
}
