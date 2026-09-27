//! Plain-text rendering of a [`ClosureDiff`], laid out like `nvd diff` for easy comparison.

use std::io::{self, Write};

use crate::diff::{ClosureDiff, PackageEntry, Selection, VersionChange};
use crate::store_path::Version;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BRIGHT_GREEN: &str = "\x1b[92m";

struct Painter {
    color: bool,
}

impl Painter {
    fn paint(&self, sgr: &str, text: &str) -> String {
        if self.color && !text.is_empty() {
            format!("{sgr}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }
}

/// Oldest first, with repeated versions collapsed to `1.0 x2`, as nvd does.
fn render_versions(versions: &[Version], p: &Painter) -> String {
    let mut groups: Vec<(&Version, usize)> = Vec::new();
    for v in versions {
        match groups.last_mut() {
            Some((last, count)) if *last == v => *count += 1,
            _ => groups.push((v, 1)),
        }
    }
    groups
        .into_iter()
        .map(|(v, count)| {
            let text = if v.text().is_empty() {
                v.to_string()
            } else {
                p.paint(YELLOW, v.text())
            };
            if count == 1 {
                text
            } else {
                format!("{text} x{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-readable signed byte count, e.g. `+50.2KiB` or `-512B`.
pub fn render_bytes(bytes: i128) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let sign = if bytes < 0 { '-' } else { '+' };
    let magnitude = bytes.unsigned_abs();
    if magnitude < 1024 {
        return format!("{sign}{magnitude}B");
    }
    let mut scaled = magnitude as f64;
    let mut unit = "B";
    for u in UNITS {
        if scaled < 1024.0 {
            break;
        }
        scaled /= 1024.0;
        unit = u;
    }
    format!("{sign}{scaled:.1}{unit}")
}

struct Row<'a> {
    state: char,
    pname: &'a str,
    selection: Selection,
    versions: String,
}

fn write_section(out: &mut impl Write, p: &Painter, title: &str, rows: &[Row]) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(out, "{}", p.paint(BOLD, title))?;
    let count_width = rows.len().to_string().len();
    let pname_width = rows
        .iter()
        .map(|r| r.pname.chars().count())
        .max()
        .unwrap_or(0);
    for (i, row) in rows.iter().enumerate() {
        let selected = row.selection.selected_anywhere();
        let pname_sgr = if selected {
            format!("{BOLD}{BRIGHT_GREEN}")
        } else {
            GREEN.to_owned()
        };
        let state = if selected {
            p.paint(BOLD, &row.state.to_string())
        } else {
            row.state.to_string()
        };
        writeln!(
            out,
            "[{state}{}]  #{:0count_width$}  {}  {}",
            p.paint(&pname_sgr, &row.selection.marker().to_string()),
            i + 1,
            p.paint(&pname_sgr, &format!("{:pname_width$}", row.pname)),
            row.versions,
        )?;
    }
    Ok(())
}

fn change_row<'a>(c: &'a VersionChange, p: &Painter) -> Row<'a> {
    Row {
        state: c.kind.marker(),
        pname: &c.pname,
        selection: c.selection,
        versions: format!(
            "{} -> {}",
            render_versions(&c.left, p),
            render_versions(&c.right, p)
        ),
    }
}

fn entry_row<'a>(state: char, e: &'a PackageEntry, p: &Painter) -> Row<'a> {
    Row {
        state,
        pname: &e.pname,
        selection: e.selection,
        versions: render_versions(&e.versions, p),
    }
}

pub fn write_diff(
    out: &mut impl Write,
    left_label: &str,
    right_label: &str,
    d: &ClosureDiff,
    color: bool,
) -> io::Result<()> {
    let p = Painter { color };
    writeln!(out, "{} {left_label}", p.paint(RED, "<<<"))?;
    writeln!(out, "{} {right_label}", p.paint(GREEN, ">>>"))?;

    let changes: Vec<Row> = d
        .version_changes
        .iter()
        .map(|c| change_row(c, &p))
        .collect();
    let selections: Vec<Row> = d
        .selection_changes
        .iter()
        .map(|e| entry_row('C', e, &p))
        .collect();
    let added: Vec<Row> = d.added.iter().map(|e| entry_row('A', e, &p)).collect();
    let removed: Vec<Row> = d.removed.iter().map(|e| entry_row('R', e, &p)).collect();
    write_section(out, &p, "Version changes:", &changes)?;
    write_section(out, &p, "Selection state changes:", &selections)?;
    write_section(out, &p, "Added packages:", &added)?;
    write_section(out, &p, "Removed packages:", &removed)?;
    if changes.is_empty() && selections.is_empty() && added.is_empty() && removed.is_empty() {
        writeln!(out, "No version or selection state changes.")?;
    }

    if !d.rebuilt.is_empty() {
        writeln!(
            out,
            "Rebuilt with unchanged versions: {} packages.",
            d.rebuilt.len()
        )?;
    }
    writeln!(
        out,
        "Closure size: {} -> {} ({} paths added, {} paths removed, delta {:+}, disk usage {}).",
        d.left_path_count,
        d.right_path_count,
        d.paths_added,
        d.paths_removed,
        d.paths_added as i64 - d.paths_removed as i64,
        render_bytes(i128::from(d.right_size) - i128::from(d.left_size)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(render_bytes(0), "+0B");
        assert_eq!(render_bytes(-1000), "-1000B");
        assert_eq!(render_bytes(51405), "+50.2KiB");
        assert_eq!(render_bytes(-3 * 1024 * 1024 * 1024), "-3.0GiB");
    }

    #[test]
    fn versions_oldest_first_with_counts() {
        let p = Painter { color: false };
        let vs: Vec<Version> = ["1.0", "1.0", "2.0"]
            .iter()
            .map(|s| Version::new(Some(s)))
            .collect();
        assert_eq!(render_versions(&vs, &p), "1.0 x2, 2.0");
        assert_eq!(render_versions(&[Version::new(None)], &p), "<none>");
    }
}
