//! The pre-check on user SQL text that names 11.4 system views: a view develop no longer has refuses the
//! migration, a view develop keeps under the same name with other columns warns.
//!
//! Input: `(object, text)` pairs — user view query specs, trigger conditions and actions, PL/CSQL source. Output:
//! refusals and warnings naming the object and the byte offset of the name in its text.
//! Rules: a name matches as a whole identifier token, case-insensitive, bare or quoted (`[..]`, `".."`, `` `..` ``);
//! no SQL is parsed, so no column-level judgement.
//! Source: decision G1 workspace#481 pre-check (refuse `db_trig`, `db_authorizations`; warn `db_user`, `db_trigger`,
//! `db_serial`: develop's `db_serial` keeps `name`/`unique_name`, its `db_user.password` is always NULL).

/// Removed in develop: a user object naming one cannot run there.
pub const REMOVED: [&str; 2] = ["db_trig", "db_authorizations"];
/// Kept in develop under the same name with changed columns.
pub const CHANGED: [&str; 3] = ["db_user", "db_trigger", "db_serial"];

#[derive(Debug, Default)]
pub struct SystemViewRefs {
    pub refusals: Vec<String>,
    pub warnings: Vec<String>,
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte offsets where `name` occurs in `text` as a whole identifier.
fn token_offsets(text: &[u8], name: &str) -> Vec<usize> {
    let n = name.len();
    (0..text.len().saturating_sub(n - 1))
        .filter(|&i| {
            text[i..i + n].eq_ignore_ascii_case(name.as_bytes())
                && (i == 0 || !is_ident(text[i - 1]))
                && text.get(i + n).is_none_or(|&b| !is_ident(b))
        })
        .collect()
}

pub fn check<'a>(texts: impl IntoIterator<Item = (String, &'a [u8])>) -> SystemViewRefs {
    let mut out = SystemViewRefs::default();
    for (object, text) in texts {
        for (names, list, what) in [
            (
                &REMOVED[..],
                &mut out.refusals,
                "which develop no longer has",
            ),
            (
                &CHANGED[..],
                &mut out.warnings,
                "whose columns differ in develop",
            ),
        ] {
            for name in names {
                let at = token_offsets(text, name);
                if !at.is_empty() {
                    list.push(format!(
                        "{object} names system view {name} ({what}) at byte {at:?}"
                    ));
                }
            }
        }
    }
    out
}
