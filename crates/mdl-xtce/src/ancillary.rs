//! Ancillary data in the MDL namespace (`spec/xtce.md` §4.1), on a
//! parameter or an aggregate parameter type: `MDL.frame` makes the
//! aggregate a composite, one vec3 or quat sample in that frame, and
//! `MDL.members` names its members in MDL's order. Other ancillary data
//! is left out.

use mdl::ir::Frame;
use mdl::source::Span;
use serde_json::Value;

use crate::Code;
use crate::report::Report;
use crate::xml::{El, Expect};

/// What an element's ancillary data in the MDL namespace says.
#[derive(Clone, Debug, Default)]
pub(crate) struct Mark {
    /// `MDL.frame`: the composite's source frame.
    pub(crate) frame: Option<(Frame, Span)>,
    /// `MDL.members`: the members in MDL's order, x, y, z or w, x, y, z.
    pub(crate) members: Option<(Vec<String>, Span)>,
}

/// No ancillary data in the MDL namespace.
pub(crate) const NO_MARK: Mark = Mark {
    frame: None,
    members: None,
};

impl Mark {
    /// This mark, with what it doesn't say taken from `fallback`, each key
    /// on its own: a parameter's frame over its type's, say, beside its
    /// type's member order.
    pub(crate) fn or(&self, fallback: &Mark) -> Mark {
        Mark {
            frame: self.frame.or(fallback.frame),
            members: self.members.clone().or_else(|| fallback.members.clone()),
        }
    }
}

/// The MDL ancillary data of `el`'s `AncillaryDataSet`, warning once about
/// any other ancillary data, which isn't imported.
pub(crate) fn mark(el: El<'_>, report: &mut Report) -> Mark {
    let mut mark = Mark::default();
    let Some(set) = el.child("AncillaryDataSet") else {
        return mark;
    };
    set.check(
        &Expect {
            attrs: &[],
            children: &["AncillaryData"],
            ignored: &[],
            text: false,
        },
        report,
    );
    let mut others = false;
    for data in set.all("AncillaryData") {
        data.check(
            &Expect {
                attrs: &["name", "mimeType", "href"],
                children: &[],
                ignored: &[],
                text: true,
            },
            report,
        );
        let Some(name) = data.required("name", report) else {
            continue;
        };
        let (text, span) = data.text();
        let twice = |report: &mut Report| {
            report.error(
                Code::Invalid,
                name.span,
                format!("{} gives {} once", el.describe(), name.value),
            );
        };
        match name.value {
            "MDL.frame" => {
                if mark.frame.is_some() {
                    twice(report);
                    continue;
                }
                match serde_json::from_value::<Frame>(Value::String(text.trim().to_string())) {
                    Ok(frame) => mark.frame = Some((frame, span)),
                    Err(_) => report.error(
                        Code::Invalid,
                        span,
                        format!(
                            "MDL.frame {:?} isn't a frame: ECEF, ECI_J2000, BODY, NED, ENU or \
                             GEODETIC",
                            text.trim()
                        ),
                    ),
                }
            }
            "MDL.members" => {
                if mark.members.is_some() {
                    twice(report);
                    continue;
                }
                let names = text.split_whitespace().map(str::to_string).collect();
                mark.members = Some((names, span));
            }
            other if other.starts_with("MDL.") => report.error(
                Code::Invalid,
                name.span,
                format!("{other} isn't MDL ancillary data, which is MDL.frame or MDL.members"),
            ),
            _ => others = true,
        }
    }
    if others {
        report.warning(
            Code::Ignored,
            set.span(),
            format!(
                "{}'s ancillary data outside the MDL namespace isn't imported",
                el.describe()
            ),
        );
    }
    mark
}
