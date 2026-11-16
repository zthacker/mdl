//! `just corpus`: every XTCE document under a directory imported, taken on
//! as a consumer takes it and decoded from synthesized payloads (see
//! `mdl_fuzz::corpus`), with a Markdown report. The directory is the first
//! argument, or `MDL_XTCE_CORPUS`; the report goes to
//! `MDL_XTCE_CORPUS_REPORT`, or `target/xtce-corpus.md`. The exit status is
//! 1 when the corpus found a bug in MDL, 2 when the run couldn't start.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = std::env::args_os()
        .nth(1)
        .or_else(|| std::env::var_os("MDL_XTCE_CORPUS"))
        .map(PathBuf::from);
    let Some(root) = root.filter(|root| root.is_dir()) else {
        eprintln!("corpus: give a directory of XTCE documents, or set MDL_XTCE_CORPUS to one");
        return ExitCode::from(2);
    };
    let report = mdl_fuzz::corpus::run(&root);
    let path = std::env::var_os("MDL_XTCE_CORPUS_REPORT")
        .map_or_else(|| PathBuf::from("target/xtce-corpus.md"), PathBuf::from);
    let written = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => std::fs::create_dir_all(dir),
        _ => Ok(()),
    }
    .and_then(|()| std::fs::write(&path, report.markdown()));
    if let Err(error) = written {
        eprintln!("corpus: can't write {}: {error}", path.display());
        return ExitCode::from(2);
    }
    println!("{}", report.summary());
    println!("report: {}", path.display());
    if report.bugs() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
