//! Parser for `git status --porcelain=v2 -z --branch`.
//!
//! Porcelain v2 with NUL termination is the only status format that is safe to
//! parse: paths are emitted raw, so a filename containing a space, a quote or a
//! newline cannot be confused with a field separator or a record boundary.
/// Where a change sits relative to the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    /// Single-letter code, matching what Git itself prints.
    pub fn code(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Copied => "C",
            ChangeKind::TypeChanged => "T",
            ChangeKind::Untracked => "?",
            ChangeKind::Conflicted => "U",
        }
    }

    fn from_status_letter(letter: u8) -> Option<Self> {
        match letter {
            b'A' => Some(ChangeKind::Added),
            b'M' => Some(ChangeKind::Modified),
            b'D' => Some(ChangeKind::Deleted),
            b'R' => Some(ChangeKind::Renamed),
            b'C' => Some(ChangeKind::Copied),
            b'T' => Some(ChangeKind::TypeChanged),
            _ => None,
        }
    }
}

/// One changed path, as it appears in one of the panel's change sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Repository-relative, so it round-trips through panel item ids and back
    /// into a Git argument without translation.
    pub path: String,
    pub kind: ChangeKind,
    /// Set for renames and copies.
    pub original_path: Option<String>,
}

/// Branch and divergence information from the `# branch.*` headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// `None` on a detached HEAD.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// True before the first commit, where `HEAD` does not resolve.
    pub is_initial: bool,
}

/// A repository's working-tree state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchStatus,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,
    pub conflicts: Vec<FileChange>,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicts.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len() + self.conflicts.len()
    }
}

/// Arguments that produce the output [`parse`] expects.
pub fn status_args() -> Vec<String> {
    [
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=normal",
        "-z",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

/// Parses `git status --porcelain=v2 -z --branch` output.
///
/// Unrecognised records are skipped rather than failing the parse: a future Git
/// adding a record type should degrade the panel, not blank it.
pub fn parse(output: &str) -> Status {
    let mut status = Status::default();
    let mut records = output.split('\0').filter(|record| !record.is_empty());

    while let Some(record) = records.next() {
        let mut chars = record.chars();
        let Some(marker) = chars.next() else { continue };
        let rest = chars.as_str().trim_start_matches(' ');

        match marker {
            '#' => parse_branch_header(rest, &mut status.branch),
            '1' => {
                if let Some((kind_x, kind_y, path)) = parse_ordinary(rest) {
                    push_change(&mut status, kind_x, kind_y, path, None);
                }
            }
            '2' => {
                // A rename or copy spends a second NUL-terminated field on the
                // original path, so it must be consumed here or every later
                // record is misread.
                let original = records.next().map(str::to_owned);
                if let Some((kind_x, kind_y, path)) = parse_rename(rest) {
                    push_change(&mut status, kind_x, kind_y, path, original);
                }
            }
            'u' => {
                if let Some(path) = parse_unmerged(rest) {
                    status.conflicts.push(FileChange {
                        path: path.to_owned(),
                        kind: ChangeKind::Conflicted,
                        original_path: None,
                    });
                }
            }
            '?' => status.untracked.push(FileChange {
                path: rest.to_owned(),
                kind: ChangeKind::Untracked,
                original_path: None,
            }),
            _ => {}
        }
    }
    status
}

fn push_change(
    status: &mut Status,
    staged: Option<ChangeKind>,
    unstaged: Option<ChangeKind>,
    path: &str,
    original_path: Option<String>,
) {
    if let Some(kind) = staged {
        status.staged.push(FileChange {
            path: path.to_owned(),
            kind,
            original_path: original_path.clone(),
        });
    }
    if let Some(kind) = unstaged {
        status.unstaged.push(FileChange {
            path: path.to_owned(),
            kind,
            // The rename is what the index records; the worktree change on top
            // of it is an ordinary modification of the new path.
            original_path: None,
        });
    }
}

fn parse_branch_header(rest: &str, branch: &mut BranchStatus) {
    let Some((key, value)) = rest.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" => branch.is_initial = value == "(initial)",
        "branch.head" => {
            branch.head = (value != "(detached)").then(|| value.to_owned());
        }
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for token in value.split_whitespace() {
                let Some((sign, count)) = token.split_at_checked(1) else {
                    continue;
                };
                let Ok(count) = count.parse::<u32>() else {
                    continue;
                };
                match sign {
                    "+" => branch.ahead = count,
                    "-" => branch.behind = count,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
fn parse_ordinary(rest: &str) -> Option<(Option<ChangeKind>, Option<ChangeKind>, &str)> {
    let (xy, remainder) = rest.split_once(' ')?;
    let path = nth_field_onwards(remainder, 6)?;
    let (staged, unstaged) = split_xy(xy)?;
    Some((staged, unstaged, path))
}

/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>`
fn parse_rename(rest: &str) -> Option<(Option<ChangeKind>, Option<ChangeKind>, &str)> {
    let (xy, remainder) = rest.split_once(' ')?;
    let path = nth_field_onwards(remainder, 7)?;
    let (staged, unstaged) = split_xy(xy)?;
    Some((staged, unstaged, path))
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
///
/// Eight fields follow `XY` before the path: the submodule marker, four modes
/// and three object names.
fn parse_unmerged(rest: &str) -> Option<&str> {
    let (_, remainder) = rest.split_once(' ')?;
    nth_field_onwards(remainder, 8)
}

/// Returns everything after `skip` space-separated fields.
///
/// The remainder is taken whole rather than split further, because it is a path
/// and a path may contain spaces.
fn nth_field_onwards(input: &str, skip: usize) -> Option<&str> {
    let mut remainder = input;
    for _ in 0..skip {
        let (_, rest) = remainder.split_once(' ')?;
        remainder = rest;
    }
    (!remainder.is_empty()).then_some(remainder)
}

/// Splits the two-letter `XY` code into its staged and unstaged halves.
///
/// `.` means unmodified on that side, which is how one path can appear in both
/// the staged and unstaged sections with different letters.
fn split_xy(xy: &str) -> Option<(Option<ChangeKind>, Option<ChangeKind>)> {
    let bytes = xy.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    Some((
        ChangeKind::from_status_letter(bytes[0]),
        ChangeKind::from_status_letter(bytes[1]),
    ))
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
