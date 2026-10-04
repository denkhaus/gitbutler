//! Select uncommitted hunks by patch text, for callers that cannot rely on short hunk IDs.
//!
//! Short hunk IDs are snapshot-scoped: their length derives from neighbouring hunks, so a
//! checkout shared with other writers can invalidate an ID between reading it and using it.
//! `but commit --patch` takes the opposite approach: the caller passes the hunks it saw, as a
//! unified diff, and every hunk must still be present byte-identically in the file's current
//! changes. The selection is therefore either exactly what the caller saw, or an error — never
//! a best-effort guess. Positions may shift (another writer moved lines elsewhere in the
//! file): hunks are matched by content, and the commit uses the current hunk positions.

use bstr::{BString, ByteSlice};
use nonempty::NonEmpty;

use crate::{CliResult, bad_input, id::UncommittedHunkOrFile};

/// One file section of the input patch, with the hunks to commit.
#[derive(Debug, PartialEq)]
pub(crate) struct PatchFile {
    /// The worktree-relative path the hunks belong to.
    pub path: BString,
    /// The hunks, in patch order.
    pub hunks: NonEmpty<PatchHunk>,
}

/// One hunk of the input patch: its body, with `@@` headers and `\` marker lines removed.
#[derive(Debug, PartialEq)]
pub(crate) struct PatchHunk {
    /// The hunk's content lines, including their `+`/`-`/` ` prefixes and terminators.
    pub body: BString,
}

/// Parse a unified diff, as `git diff` writes it, into per-file hunk selections.
///
/// Both full `diff --git a/x b/x` sections and bare `--- a/x` / `+++ b/x` sections are
/// accepted; the `b` side names the path (the `a` side for deletions). File headers are only
/// recognized before the first `@@` of a section, so removed lines like `--- x` inside a hunk
/// body stay body lines.
/// The parse state of one file section of the patch.
#[derive(Default)]
struct Section {
    path: Option<BString>,
    hunks: Vec<PatchHunk>,
    open_hunk: Option<BString>,
}

impl Section {
    /// File headers are meaningful only while no hunk is open.
    fn header_position(&self) -> bool {
        self.open_hunk.is_none() && self.hunks.is_empty()
    }
}

pub(crate) fn parse(patch: &[u8]) -> CliResult<Vec<PatchFile>> {
    let mut files = Vec::<PatchFile>::new();
    let mut section = Section::default();
    for line in patch.lines_with_terminator() {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            section = Section::take(&mut files, section);
            let _ = rest; // The path is read from the `+++`/`---` header below.
            continue;
        }
        if section.header_position() && line.starts_with(b"--- ") {
            section = Section::take(&mut files, section);
            if let Some(path) = path_after_marker(line, b"--- ")
                && path != b"/dev/null"
            {
                section.path = Some(path);
            }
            continue;
        }
        if section.header_position() && line.starts_with(b"+++ ") {
            if let Some(path) = path_after_marker(line, b"+++ ")
                && path != b"/dev/null"
            {
                section.path = Some(path);
            }
            continue;
        }
        if line.starts_with(b"@@") {
            if let Some(body) = section.open_hunk.take() {
                section.hunks.push(PatchHunk { body });
            }
            section.open_hunk = Some(BString::default());
            continue;
        }
        if let Some(body) = section.open_hunk.as_mut() {
            // `\` lines are the "\ No newline at end of file" marker, which diff engines
            // disagree on writing; content lines are ` `, `+` and `-` prefixed either way.
            if !line.starts_with(b"\\") {
                body.extend_from_slice(line);
            }
        } else if section.header_position() {
            // Metadata like `index abc..def`, `new file`, `similarity index`: no hunk yet.
        } else {
            return Err(bad_input(format!(
                "patch line outside of a hunk: {:?}",
                line.as_bstr()
            ))
            .into());
        }
    }
    Section::take(&mut files, section);

    if files.is_empty() {
        return Err(bad_input("the patch contains no file with hunks").into());
    }
    Ok(files)
}

impl Section {
    /// Close `section`: a file without hunks is dropped (`git diff` emits those for mode-only
    /// changes and pure renames, which have no hunk text to match).
    fn take(files: &mut Vec<PatchFile>, section: Section) -> Section {
        let Section {
            path,
            hunks,
            open_hunk,
        } = section;
        let mut hunks = hunks;
        if let Some(body) = open_hunk {
            hunks.push(PatchHunk { body });
        }
        if let (Some(path), Some(hunks)) = (path, NonEmpty::from_vec(hunks)) {
            if let Some(existing) = files.iter_mut().find(|file| file.path == path) {
                // `git diff` writes one section per file, but a hand-assembled patch may
                // repeat one; both selections commit the same file.
                existing.hunks.extend(hunks);
            } else {
                files.push(PatchFile { path, hunks });
            }
        }
        Section::default()
    }
}

/// The path of a `--- `/`+++ ` header line, without the `a/`/`b/` prefixes git adds.
fn path_after_marker(line: &[u8], marker: &[u8]) -> Option<BString> {
    let path = line.strip_prefix(marker)?.strip_suffix(b"\n")?;
    let path = path.strip_suffix(b"\r").unwrap_or(path);
    // A tab separates path and timestamp when diff prefixes are disabled; we require the
    // prefixed form and take everything after `a/` or `b/` verbatim.
    let path = path
        .strip_prefix(b"a/")
        .or_else(|| path.strip_prefix(b"b/"))?;
    Some(BString::from(path))
}

/// The content lines of a hunk's unified diff, the same basis [`parse`] collects: `@@` header
/// and `\` marker lines removed, everything else byte-identical.
fn hunk_body(diff: &BString) -> BString {
    let mut body = BString::default();
    for line in diff.lines_with_terminator() {
        if !line.starts_with(b"@@") && !line.starts_with(b"\\") {
            body.extend_from_slice(line);
        }
    }
    body
}

/// Match each patch hunk against the current uncommitted changes of its file.
///
/// Matching is by content: line numbers may have shifted since the patch was captured, but
/// the content lines must match a current hunk of the same file byte-for-byte, context lines
/// included. Each match selects the *current* hunk, so the commit addresses current
/// positions. A hunk with no match, or with several indistinguishable matches, is an error:
/// the caller decides how to react, the commit never guesses.
pub(crate) fn match_current_hunks(
    files: Vec<PatchFile>,
    current: &[UncommittedHunkOrFile],
) -> CliResult<Vec<UncommittedHunkOrFile>> {
    let mut selected = Vec::new();
    for file in files {
        let Some(current_file) = current
            .iter()
            .find(|current| current.hunks.first().hunk.path == file.path)
        else {
            return Err(patch_mismatch(&file, "the file has no uncommitted changes"));
        };

        let mut matched = Vec::new();
        for patch_hunk in &file.hunks {
            let body = &patch_hunk.body;
            let hits = current_file
                .hunks
                .iter()
                .filter(|id_and_hunk| {
                    id_and_hunk
                        .hunk
                        .diff
                        .as_ref()
                        .is_some_and(|diff| &hunk_body(diff) == body)
                })
                .collect::<Vec<_>>();
            match hits.as_slice() {
                [single] => matched.push((*single).clone()),
                [] => {
                    return Err(patch_mismatch(
                        &file,
                        "a hunk of the patch is not among the file's current changes; \
                         the file changed since the patch was captured",
                    ));
                }
                _ => {
                    return Err(patch_mismatch(
                        &file,
                        "a hunk of the patch matches several identical current hunks; \
                         select them by hunk ID from `but diff` instead",
                    ));
                }
            }
        }
        let Some(hunks) = NonEmpty::from_vec(matched) else {
            return Err(bad_input(format!("no hunks selected for {}", file.path.as_bstr())).into());
        };
        selected.push(UncommittedHunkOrFile {
            id: current_file.id.clone(),
            hunks,
            // A patch selects hunks, never a file as a whole.
            is_entire_file: false,
            source: current_file.source.clone(),
        });
    }
    Ok(selected)
}

/// The error for a patch that does not describe the current changes.
fn patch_mismatch(file: &PatchFile, reason: &str) -> crate::CliError {
    bad_input(format!(
        "cannot commit patch hunks of {}: {reason}",
        file.path.as_bstr()
    ))
    .hint("re-run `but diff <file>` and rebuild the patch from what it shows now")
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::IdAndHunk;

    fn parse_one(patch: &str) -> PatchFile {
        let mut files = parse(patch.as_bytes()).unwrap();
        assert_eq!(files.len(), 1, "one file section: {files:?}");
        files.pop().unwrap()
    }

    #[test]
    fn parse_reads_git_style_sections() {
        let file = parse_one(
            "diff --git a/notes.md b/notes.md\nindex 123..456 100644\n--- a/notes.md\n+++ b/notes.md\n@@ -1,3 +1,4 @@\n base\n-old\n+new\n tail\n",
        );
        assert_eq!(file.path, BString::from("notes.md"));
        assert_eq!(file.hunks.len(), 1);
        assert_eq!(
            file.hunks[0].body,
            BString::from(" base\n-old\n+new\n tail\n")
        );
    }

    #[test]
    fn parse_reads_bare_unified_diffs_and_deletions() {
        let file = parse_one("--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-all\n");
        assert_eq!(file.path, BString::from("gone.txt"));
        assert_eq!(file.hunks[0].body, BString::from("-all\n"));
    }

    #[test]
    fn parse_keeps_removed_marker_lines_inside_hunks() {
        let file = parse_one(
            "--- a/log\n+++ b/log\n@@ -1,2 +1,2 @@\n---- looks like a header\n+--- but is content\n",
        );
        assert_eq!(
            file.hunks[0].body,
            BString::from("---- looks like a header\n+--- but is content\n")
        );
    }

    #[test]
    fn parse_drops_no_newline_markers_and_merges_repeated_files() {
        let file = parse_one(
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n@@ -5 +5 @@\n-c\n+d\n",
        );
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].body, BString::from("-a\n+b\n"));
        assert_eq!(file.hunks[1].body, BString::from("-c\n+d\n"));
    }

    #[test]
    fn parse_rejects_patches_without_hunks() {
        assert!(parse(b"diff --git a/x b/x\n").is_err());
        assert!(parse(b"").is_err());
    }

    fn current_file(hunks: &[(u32, &str, &str)]) -> UncommittedHunkOrFile {
        let hunks = hunks
            .iter()
            .map(|(old_start, header, diff)| IdAndHunk {
                id: String::new(),
                hunk: but_core::SingleHunk {
                    hunk_header: Some(but_core::HunkHeader {
                        old_start: *old_start,
                        old_lines: 1,
                        new_start: 1,
                        new_lines: 1,
                    }),
                    path: BString::from("notes.md"),
                    diff: Some(BString::from(format!("{header}\n{diff}").into_bytes())),
                },
                tree_status: but_core::TreeStatusKind::Modification,
            })
            .collect::<Vec<_>>();
        UncommittedHunkOrFile {
            id: "f".to_string(),
            hunks: NonEmpty::from_vec(hunks).unwrap(),
            is_entire_file: true,
            source: crate::utils::change_source::ChangeSourceId::Head,
        }
    }

    #[test]
    fn matching_is_by_content_not_position() {
        let file = parse_one(
            "--- a/notes.md\n+++ b/notes.md\n@@ -1,3 +1,4 @@\n base\n-old\n+new\n tail\n",
        );
        // The current hunk sits at different line numbers, same content.
        let current = current_file(&[
            (40, "@@ -40,3 +41,4 @@", " base\n-old\n+new\n tail\n"),
            (9, "@@ -9,1 +9,2 @@", " other\n"),
        ]);
        let selected = match_current_hunks(vec![file], &[current]).unwrap();
        assert_eq!(selected.len(), 1);
        assert!(!selected[0].is_entire_file, "a patch selects hunks only");
        assert_eq!(selected[0].hunks.len(), 1);
        assert_eq!(
            selected[0].hunks[0].hunk.hunk_header.unwrap().old_start,
            40,
            "the selected hunk carries the current position"
        );
    }

    #[test]
    fn matching_rejects_stale_and_ambiguous_hunks() {
        let stale = parse_one("--- a/notes.md\n+++ b/notes.md\n@@ -1,2 +1,2 @@\n-x\n+y\n");
        assert!(
            match_current_hunks(vec![stale], &[current_file(&[(1, "@@ -1 +1 @@", " a\n")])])
                .is_err()
        );

        let file = parse_one("--- a/notes.md\n+++ b/notes.md\n@@ -1 +1 @@\n-a\n+b\n");
        let duplicate_bodies = current_file(&[
            (1, "@@ -1 +1 @@", "-a\n+b\n"),
            (8, "@@ -8 +8 @@", "-a\n+b\n"),
        ]);
        assert!(
            match_current_hunks(vec![file], &[duplicate_bodies]).is_err(),
            "identical current hunks are ambiguous"
        );
    }
}
