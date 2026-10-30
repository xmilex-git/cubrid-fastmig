//! The catalog templates committed under `crates/catalog/template/`, compiled into the binary so a migration
//! needs no engine at run time.
//!
//! Input: the target line and the database charset (volume header `db_charset`). Output: the parsed template.
//! Source: ADR 0011; templates made by `crates/catalog/template/build.sh` (see the README there).

use fastmig_ondisk::VersionLine;
use fastmig_ondisk::value::string::codeset;

use crate::template::CatalogTemplate;

const DEVELOP_ISO88591: &str = include_str!("../template/develop-iso88591.tpl");
const DEVELOP_EUCKR: &str = include_str!("../template/develop-euckr.tpl");
const DEVELOP_UTF8: &str = include_str!("../template/develop-utf8.tpl");
const V11_4_ISO88591: &str = include_str!("../template/11.4-iso88591.tpl");
const V11_4_EUCKR: &str = include_str!("../template/11.4-euckr.tpl");
const V11_4_UTF8: &str = include_str!("../template/11.4-utf8.tpl");

/// The template for `line` and `charset`; `None` when no template exists for the pair (develop and 11.4 targets have
/// templates, workspace#533).
pub fn builtin(line: VersionLine, charset: u8) -> Option<CatalogTemplate> {
    let text = match (line, charset) {
        (VersionLine::Develop, codeset::ISO88591) => DEVELOP_ISO88591,
        (VersionLine::Develop, codeset::KSC5601_EUC) => DEVELOP_EUCKR,
        (VersionLine::Develop, codeset::UTF8) => DEVELOP_UTF8,
        (VersionLine::V11_4, codeset::ISO88591) => V11_4_ISO88591,
        (VersionLine::V11_4, codeset::KSC5601_EUC) => V11_4_EUCKR,
        (VersionLine::V11_4, codeset::UTF8) => V11_4_UTF8,
        _ => return None,
    };
    // a committed template that does not parse is a build defect, not an input error
    Some(CatalogTemplate::parse(text).expect("committed catalog template parses"))
}
