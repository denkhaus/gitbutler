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

use crate::{
    CliResult, bad_input,
    id::{IdAndHunk, UncommittedHunkOrFile},
};

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
                    let Some(window) =
                        match_window_inside_current_hunks(&file, body, current_file)?
                    else {
                        return Err(patch_mismatch(
                            &file,
                            "a hunk of the patch is not among the file's current changes; \
                             the file changed since the patch was captured",
                        ));
                    };
                    matched.push(window);
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

/// The kind of change a patch hunk selects once its context lines are ignored.
enum WindowKind {
    /// Every non-context line of the hunk adds a line.
    Addition,
    /// Every non-context line of the hunk removes a line.
    Deletion,
}

/// Classify a patch hunk's lines as a pure [`WindowKind`], if it is one.
///
/// Replacements that remove and add lines in one hunk, and hunks without any change line,
/// are not expressible as a sub-hunk selection and return `None`.
fn window_kind(lines: &[&[u8]]) -> Option<WindowKind> {
    let mut additions = 0;
    let mut removals = 0;
    for line in lines {
        match line.first() {
            Some(b'+') => additions += 1,
            Some(b'-') => removals += 1,
            Some(b' ') | None => {}
            _ => return None,
        }
    }
    match (additions, removals) {
        (0, 0) => None,
        (0, _) => Some(WindowKind::Deletion),
        (_, 0) => Some(WindowKind::Addition),
        _ => None,
    }
}

/// Match a patch hunk as a contiguous window inside a current hunk, on the old side.
///
/// Concurrent writers fuse neighbouring changes into one hunk, so a patch captured earlier may
/// no longer match a whole current hunk even though what it shows is still there: its old-side
/// lines - context and removals - still appear contiguously, in order, in the current hunk's
/// old side. Such a patch hunk matches the current hunk that contains its old side as a
/// contiguous window, and the selection addresses only the window's own change - the lines it
/// adds or removes, positioned where the *current* hunk has them, as a zero-length-range hunk
/// header the commit path stages as a sub-hunk selection.
///
/// Only windows that purely add or purely remove lines are expressible; replacements inside a
/// hunk are not. Windows are matched by content in every current hunk of the file: none is a
/// stale no-match, and more than one is ambiguous - both fail closed, like whole-hunk matching.
fn match_window_inside_current_hunks(
    file: &PatchFile,
    body: &BString,
    current_file: &UncommittedHunkOrFile,
) -> CliResult<Option<IdAndHunk>> {
    let patch_lines = body.lines_with_terminator().collect::<Vec<_>>();
    if patch_lines.is_empty() {
        return Ok(None);
    }
    let Some(kind) = window_kind(&patch_lines) else {
        return Ok(None);
    };
    let patch_old_side = old_side_lines(&patch_lines);
    if patch_old_side.is_empty() {
        // Without old-side lines there is no position to anchor the window to.
        return Ok(None);
    }

    // (index into the file's current hunks, offset of the window in that hunk's old side).
    let mut windows = Vec::<(usize, usize)>::new();
    for (hunk_index, id_and_hunk) in current_file.hunks.iter().enumerate() {
        let Some(diff) = id_and_hunk.hunk.diff.as_ref() else {
            continue;
        };
        let current_body = hunk_body(diff);
        let current_lines = current_body.lines_with_terminator().collect::<Vec<_>>();
        let current_old_side = old_side_lines(&current_lines);
        if current_old_side.len() < patch_old_side.len() {
            continue;
        }
        for (offset, window) in current_old_side.windows(patch_old_side.len()).enumerate() {
            if window == patch_old_side.as_slice() {
                windows.push((hunk_index, offset));
            }
        }
    }

    match windows.as_slice() {
        [] => Ok(None),
        [(hunk_index, offset)] => {
            let id_and_hunk = current_file
                .hunks
                .get(*hunk_index)
                .expect("BUG: the window matched a hunk of this very file");
            let Some(diff) = id_and_hunk.hunk.diff.as_ref() else {
                return Ok(None);
            };
            let current_body = hunk_body(diff);
            let current_lines = current_body.lines_with_terminator().collect::<Vec<_>>();
            Ok(Some(select_window(
                id_and_hunk,
                &current_lines,
                &patch_lines,
                *offset,
                kind,
            )))
        }
        _ => Err(patch_mismatch(
            file,
            "a hunk of the patch matches several identical windows of the current changes; \
             add context lines to tell them apart",
        )),
    }
}

/// The lines of a hunk body as they appear on its old side: context and removals, in order.
fn old_side_lines<'a>(lines: &'a [&'a [u8]]) -> Vec<&'a [u8]> {
    lines
        .iter()
        .filter(|line| line.starts_with(b" ") || line.starts_with(b"-"))
        .copied()
        .collect()
}

/// Turn a matched old-side window into a selection of only its change lines.
///
/// The selection reuses the current hunk's identity and rewrites its header to address just
/// the window's added or removed lines: a null old range for additions, a null new range for
/// removals, with the line numbers the *current* hunk gives them. For additions, the new-file
/// position is where the current hunk places the old-side point the window inserts at; for
/// removals, the old-file position is the window's own run in the current old side.
fn select_window(
    id_and_hunk: &IdAndHunk,
    current_lines: &[&[u8]],
    patch_lines: &[&[u8]],
    window_offset: usize,
    kind: WindowKind,
) -> IdAndHunk {
    let Some(header) = id_and_hunk.hunk.hunk_header else {
        // No header, no position to address; the caller treats this as no match.
        return id_and_hunk.clone();
    };
    // Patch old-side lines before the window's first change line, locating the change within
    // the window.
    let patch_change = patch_lines
        .iter()
        .position(|line| !line.starts_with(b" "))
        .expect("BUG: a matched window has a change line");
    let old_side_before_change = patch_lines
        .get(..patch_change)
        .map(|lines| old_side_lines(lines).len())
        .unwrap_or(0);
    let change_line_count = patch_lines
        .iter()
        .filter(|line| line.starts_with(select_side_prefix(&kind)))
        .count();

    let mut selection = id_and_hunk.clone();
    selection.hunk.hunk_header = Some(match kind {
        WindowKind::Addition => {
            // Walk the current hunk's lines until its old side reaches the point the window
            // inserts at; the new-side lines seen until there are its new-file position.
            let target_old_side = window_offset + old_side_before_change;
            let mut old_side_seen = 0;
            let mut new_side_seen = 0;
            for line in current_lines {
                if old_side_seen == target_old_side {
                    break;
                }
                match line.first() {
                    Some(b'+') => new_side_seen += 1,
                    _ => {
                        old_side_seen += 1;
                        new_side_seen += 1;
                    }
                }
            }
            but_core::HunkHeader {
                old_start: 0,
                old_lines: 0,
                new_start: header.new_start + new_side_seen as u32,
                new_lines: u32::try_from(change_line_count).unwrap_or(u32::MAX),
            }
        }
        WindowKind::Deletion => but_core::HunkHeader {
            old_start: header.old_start + (window_offset + old_side_before_change) as u32,
            old_lines: u32::try_from(change_line_count).unwrap_or(u32::MAX),
            new_start: 0,
            new_lines: 0,
        },
    });
    selection
}

/// The diff line prefix of the lines a [`WindowKind`] selects.
fn select_side_prefix(kind: &WindowKind) -> &[u8] {
    match kind {
        WindowKind::Addition => b"+",
        WindowKind::Deletion => b"-",
    }
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

    fn current_file_with_headers(hunks: &[(u32, u32, u32, u32, &str)]) -> UncommittedHunkOrFile {
        let hunks = hunks
            .iter()
            .map(
                |(old_start, old_lines, new_start, new_lines, diff)| IdAndHunk {
                    id: String::new(),
                    hunk: but_core::SingleHunk {
                        hunk_header: Some(but_core::HunkHeader {
                            old_start: *old_start,
                            old_lines: *old_lines,
                            new_start: *new_start,
                            new_lines: *new_lines,
                        }),
                        path: BString::from("notes.md"),
                        diff: Some(BString::from(
                            format!(
                                "@@ -{old_start},{old_lines} +{new_start},{new_lines} @@\n{diff}"
                            )
                            .into_bytes(),
                        )),
                    },
                    tree_status: but_core::TreeStatusKind::Modification,
                },
            )
            .collect::<Vec<_>>();
        UncommittedHunkOrFile {
            id: "f".to_string(),
            hunks: NonEmpty::from_vec(hunks).unwrap(),
            is_entire_file: true,
            source: crate::utils::change_source::ChangeSourceId::Head,
        }
    }

    #[test]
    fn window_matching_selects_an_addition_inside_a_fused_hunk() {
        let file = parse_one(
            "--- a/notes.md\n+++ b/notes.md\n@@ -1,2 +1,3 @@\n seq 19\n seq 20\n+seq 21\n",
        );
        // A neighbour appended below, so the writer's addition is now the prefix window of a
        // fused hunk: context + both additions.
        let current =
            current_file_with_headers(&[(1, 2, 1, 4, " seq 19\n seq 20\n+seq 21\n+seq 22\n")]);

        let selected = match_current_hunks(vec![file], &[current]).unwrap();
        let hunk_header = selected[0].hunks[0].hunk.hunk_header.unwrap();
        assert_eq!(
            (
                hunk_header.old_start,
                hunk_header.old_lines,
                hunk_header.new_start,
                hunk_header.new_lines
            ),
            (0, 0, 3, 1),
            "the selection addresses only the writer's added line, as an additive window"
        );
    }

    #[test]
    fn window_matching_selects_a_deletion_next_to_another_writers_addition() {
        let file = parse_one(
            "--- a/notes.md\n+++ b/notes.md\n@@ -1,4 +1,3 @@\n seq 19\n-seq 20\n seq 21\n",
        );
        // A neighbour's addition fuses into the same hunk; it never appears on the old side,
        // so the writer's deletion and its context still match contiguously there.
        let current =
            current_file_with_headers(&[(1, 3, 1, 3, " seq 19\n-seq 20\n seq 21\n+seq 25\n")]);

        let selected = match_current_hunks(vec![file], &[current]).unwrap();
        let hunk_header = selected[0].hunks[0].hunk.hunk_header.unwrap();
        assert_eq!(
            (
                hunk_header.old_start,
                hunk_header.old_lines,
                hunk_header.new_start,
                hunk_header.new_lines
            ),
            (2, 1, 0, 0),
            "the selection addresses only the writer's removed line, as a deletive window"
        );
    }

    #[test]
    fn window_matching_selects_a_mid_file_addition_with_trailing_context() {
        let file =
            parse_one("--- a/notes.md\n+++ b/notes.md\n@@ -1,2 +1,3 @@\n alpha\n+beta\n gamma\n");
        // The neighbour appended right below, so on the old side the writer's trailing
        // context still directly follows its leading context.
        let current = current_file_with_headers(&[(1, 2, 1, 4, " alpha\n+beta\n+beta2\n gamma\n")]);

        let selected = match_current_hunks(vec![file], &[current]).unwrap();
        let hunk_header = selected[0].hunks[0].hunk.hunk_header.unwrap();
        assert_eq!(
            (
                hunk_header.old_start,
                hunk_header.old_lines,
                hunk_header.new_start,
                hunk_header.new_lines
            ),
            (0, 0, 2, 1),
            "the selection addresses only the writer's added line between its context"
        );
    }

    #[test]
    fn window_matching_rejects_mixed_add_and_remove_windows() {
        let file = parse_one(
            "--- a/notes.md\n+++ b/notes.md\n@@ -1,4 +1,4 @@\n seq 19\n-seq 20\n+seq 21\n seq 22\n",
        );
        let current = current_file_with_headers(&[(
            1,
            4,
            1,
            4,
            " seq 19\n-seq 20\n+seq 21\n+seq 22\n seq 23\n",
        )]);
        assert!(
            match_current_hunks(vec![file], &[current]).is_err(),
            "a replacement window is not expressible, so it must fail closed"
        );
    }

    #[test]
    fn window_matching_rejects_ambiguous_windows() {
        let file = parse_one("--- a/notes.md\n+++ b/notes.md\n@@ -1 +2 @@\n x\n+same row\n");
        // The old-side context line repeats, so the window matches at both of its positions.
        let current = current_file_with_headers(&[(1, 2, 1, 4, " x\n+other row\n x\n+same row\n")]);
        assert!(
            match_current_hunks(vec![file], &[current]).is_err(),
            "two indistinguishable windows must fail closed"
        );
    }

    #[test]
    fn window_matching_rejects_context_free_windows() {
        let file = parse_one("--- a/notes.md\n+++ b/notes.md\n@@ -1 +1 @@\n+seq 21\n");
        let current = current_file_with_headers(&[(1, 1, 1, 2, " base\n+seq 21\n")]);
        assert!(
            match_current_hunks(vec![file], &[current]).is_err(),
            "without old-side lines there is no position to anchor the window to"
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
