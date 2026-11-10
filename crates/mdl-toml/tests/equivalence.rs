//! The same bundle written in TOML and in JSON builds the same artifacts,
//! whether the TOML is one file or many.

use mdl::artifact::{Artifacts, build};
use mdl::source::{Parsed, Sources};
use mdl::validate::validate;
use serde_json::{Map, Value, json};

const JSON: &str = include_str!("../../mdl/tests/fixtures/full_bundle.json");
const TOML: &str = include_str!("fixtures/full_bundle.toml");

fn artifacts(parsed: Parsed) -> Artifacts {
    build(&validate(parsed.bundle).unwrap()).unwrap()
}

fn from_json() -> Artifacts {
    let mut sources = Sources::new();
    let id = sources.add("full_bundle.json", JSON);
    artifacts(mdl::json::read(sources.get(id).unwrap()).unwrap())
}

fn from_toml(files: &[(String, String)]) -> Artifacts {
    let mut sources = Sources::new();
    for (name, text) in files {
        sources.add(name.as_str(), text.as_str());
    }
    match mdl_toml::read(sources.iter()) {
        Ok(parsed) => artifacts(parsed),
        Err(errors) => {
            let shown: Vec<String> = errors
                .iter()
                .map(|e| e.display(&sources).to_string())
                .collect();
            panic!("{}", shown.join("\n"));
        }
    }
}

/// `part` as one TOML file of a bundle.
fn file(name: &str, part: Map<String, Value>) -> (String, String) {
    let mut document = Map::new();
    document.insert("mdl_version".to_string(), json!("2.0"));
    document.extend(part);
    (name.to_string(), toml::to_string(&document).unwrap())
}

#[test]
fn the_full_bundle_builds_the_same_artifacts_from_toml_as_from_json() {
    let toml = from_toml(&[("full_bundle.toml".to_string(), TOML.to_string())]);
    assert_eq!(toml, from_json());
}

#[test]
fn split_across_files_it_builds_the_same_artifacts() {
    let bundle: Value = serde_json::from_str(JSON).unwrap();
    let mut files = Vec::new();
    // Params and commands in separate files, one family and one set per file.
    for part in ["params", "commands"] {
        let mut catalog = Map::new();
        catalog.insert(part.to_string(), bundle["catalog"][part].clone());
        let mut top = Map::new();
        top.insert("catalog".to_string(), Value::Object(catalog));
        files.push(file(&format!("catalog_{part}.toml"), top));
    }
    for list in ["families", "sets"] {
        for (index, item) in bundle[list].as_array().unwrap().iter().enumerate() {
            let mut top = Map::new();
            top.insert(list.to_string(), json!([item]));
            files.push(file(&format!("{list}_{index}.toml"), top));
        }
    }
    assert!(files.len() > 6);
    assert_eq!(from_toml(&files), from_json());
    // File order doesn't change any identity: artifacts list by name.
    files.reverse();
    assert_eq!(from_toml(&files), from_json());
}
