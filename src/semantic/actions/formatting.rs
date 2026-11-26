use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail, ensure};
use lsp_types::TextEdit;

use crate::semantic::snapshot::DocumentSnapshot;

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "document formatting request entry point")
)]
pub(crate) fn format_document(
    document: &DocumentSnapshot<'_>,
    workdir: Option<&Path>,
) -> Result<Vec<TextEdit>> {
    if document.root_node().has_error() {
        return Ok(Vec::new());
    }
    let executable = gofmt_path(workdir);
    let mut command = Command::new(&executable);
    if let Some(dir) = workdir {
        command.current_dir(dir);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting gofmt ({})", executable.display()))?;
    let mut stdin = child.stdin.take().expect("gofmt stdin is piped");
    let text = document.text();
    let (written, output) = thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(text.as_bytes()));
        let output = child.wait_with_output();
        (writer.join().expect("gofmt stdin writer panicked"), output)
    });
    let output = output.context("waiting for gofmt")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A Go line directive can give a stdin parse error a different filename.
        if stderr.lines().any(|line| {
            line.split_once(": ")
                .and_then(|(position, _)| position.rsplit_once(':'))
                .is_some_and(|(_, number)| number.parse::<u32>().is_ok())
        }) {
            return Ok(Vec::new());
        }
        bail!("gofmt failed ({}): {}", output.status, stderr.trim_end());
    }
    written.context("writing document to gofmt")?;
    formatting_edits(
        document,
        String::from_utf8(output.stdout).context("decoding gofmt output")?,
    )
}

fn gofmt_path(workdir: Option<&Path>) -> PathBuf {
    fn from_root(root: &Path) -> Option<PathBuf> {
        let path = root
            .join("bin")
            .join(format!("gofmt{}", env::consts::EXE_SUFFIX));
        (root.is_absolute() && path.is_file()).then_some(path)
    }

    if let Some(path) = env::var_os("GOROOT").and_then(|root| from_root(Path::new(&root))) {
        return path;
    }
    let mut go = Command::new("go");
    if let Some(dir) = workdir {
        go.current_dir(dir);
    }
    if let Ok(output) = go.args(["env", "GOROOT"]).output()
        && output.status.success()
        && let Ok(root) = std::str::from_utf8(&output.stdout)
        && let Some(path) = from_root(Path::new(root.trim_end_matches(['\r', '\n'])))
    {
        return path;
    }
    PathBuf::from("gofmt")
}

pub(super) fn formatting_edits(
    document: &DocumentSnapshot<'_>,
    mut formatted: String,
) -> Result<Vec<TextEdit>> {
    let original = document.text();
    if formatted == original {
        return Ok(Vec::new());
    }
    ensure!(
        !formatted.trim().is_empty() || original.trim().is_empty(),
        "gofmt returned an empty result for a non-empty document"
    );
    if original.matches("\r\n").count() > original.bytes().filter(|&b| b == b'\n').count() / 2 {
        formatted = formatted.replace("\r\n", "\n").replace('\n', "\r\n");
        if formatted == original {
            return Ok(Vec::new());
        }
    }

    let mut start = matching_bytes(original.chars(), formatted.chars());
    if splits_crlf(original, start) || splits_crlf(&formatted, start) {
        start -= 1;
    }
    let mut suffix = matching_bytes(
        original[start..].chars().rev(),
        formatted[start..].chars().rev(),
    );
    // LSP has no position between the two bytes of a CRLF.
    if splits_crlf(original, original.len() - suffix)
        || splits_crlf(&formatted, formatted.len() - suffix)
    {
        suffix -= 1;
    }
    Ok(vec![TextEdit::new(
        document.byte_range(start..original.len() - suffix),
        formatted[start..formatted.len() - suffix].to_owned(),
    )])
}

fn matching_bytes(left: impl Iterator<Item = char>, right: impl Iterator<Item = char>) -> usize {
    left.zip(right)
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum()
}

fn splits_crlf(text: &str, offset: usize) -> bool {
    text[..offset].ends_with('\r') && text[offset..].starts_with('\n')
}
