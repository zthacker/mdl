//! MDL's XTCE importer: OMG XTCE space system definitions (versions 1.0
//! to 1.2) read as an MDL bundle, as `spec/xtce.md` in the MDL repository
//! describes.
//!
//! XTCE describes whole packets. [`read`] makes each concrete sequence
//! container a packet of one set, each parameter in it a field and a
//! catalog param, and the restriction criteria of the containers it
//! inherits from its identify keys. Aggregates become one field per
//! member. Whatever the XTCE says that MDL can't represent is reported,
//! never dropped: as an error when leaving it out would change a decoded
//! value, as a warning otherwise.
//!
//! ```
//! use mdl::v2::source::Sources;
//!
//! let mut sources = Sources::new();
//! sources.add("sat.xml", r#"
//! <SpaceSystem name="Sat" xmlns="http://www.omg.org/spec/XTCE/20180204">
//!   <TelemetryMetaData>
//!     <ParameterTypeSet>
//!       <IntegerParameterType name="Id" signed="false">
//!         <IntegerDataEncoding sizeInBits="8"/>
//!       </IntegerParameterType>
//!       <FloatParameterType name="Volts">
//!         <UnitSet><Unit>V</Unit></UnitSet>
//!         <IntegerDataEncoding sizeInBits="16">
//!           <DefaultCalibrator>
//!             <PolynomialCalibrator>
//!               <Term coefficient="0" exponent="0"/>
//!               <Term coefficient="0.001" exponent="1"/>
//!             </PolynomialCalibrator>
//!           </DefaultCalibrator>
//!         </IntegerDataEncoding>
//!       </FloatParameterType>
//!     </ParameterTypeSet>
//!     <ParameterSet>
//!       <Parameter name="PacketId" parameterTypeRef="Id"/>
//!       <Parameter name="BusVoltage" parameterTypeRef="Volts"/>
//!     </ParameterSet>
//!     <ContainerSet>
//!       <SequenceContainer name="Header" abstract="true">
//!         <EntryList><ParameterRefEntry parameterRef="PacketId"/></EntryList>
//!       </SequenceContainer>
//!       <SequenceContainer name="Power">
//!         <EntryList><ParameterRefEntry parameterRef="BusVoltage"/></EntryList>
//!         <BaseContainer containerRef="Header">
//!           <RestrictionCriteria>
//!             <Comparison parameterRef="PacketId" value="7"/>
//!           </RestrictionCriteria>
//!         </BaseContainer>
//!       </SequenceContainer>
//!     </ContainerSet>
//!   </TelemetryMetaData>
//! </SpaceSystem>
//! "#);
//! let parsed = mdl_xtce::read(sources.iter(), &mdl_xtce::Options::default()).unwrap();
//! let packet = &parsed.bundle.sets[0].packets[0];
//! assert_eq!(packet.name, "Power");
//! assert!(parsed.bundle.catalog.params.contains_key("sat.busvoltage"));
//! ```

use mdl::v2::ir::Locator;
use mdl::v2::read::read_bundle_value;
use mdl::v2::source::{Parsed, SourceDiagnostic, SourceFile};

mod import;
mod names;
mod out;
mod report;
mod space;
mod types;
mod units;
mod xml;

#[cfg(test)]
mod tests;

use import::Import;
use report::Report;
use space::Tree;
use xml::El;

/// What an XTCE diagnostic is about. The codes are stable once released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Code {
    /// Text that isn't well-formed XML, or has a DTD.
    Syntax,
    /// A document that isn't XTCE: its root isn't a SpaceSystem in an
    /// XTCE namespace. Also no documents at all.
    Document,
    /// Something XTCE doesn't allow: a missing attribute, a value that
    /// isn't a number, a container that inherits from itself.
    Invalid,
    /// A reference that names nothing.
    Reference,
    /// Two items of one kind with one name in a SpaceSystem, or two root
    /// SpaceSystems with one name.
    Duplicate,
    /// A name that can't become an MDL name, or two that become the same.
    Name,
    /// Something MDL can't represent, whose absence would change a decoded
    /// value. The bundle isn't built.
    Unsupported,
    /// Something MDL doesn't keep that doesn't change decoded values: a
    /// warning.
    Ignored,
    /// An XTCE name changed to fit MDL's rules for names: a warning.
    Renamed,
}

impl Code {
    /// Every code, for tools that map a diagnostic's code back.
    pub const ALL: &'static [Code] = &[
        Code::Syntax,
        Code::Document,
        Code::Invalid,
        Code::Reference,
        Code::Duplicate,
        Code::Name,
        Code::Unsupported,
        Code::Ignored,
        Code::Renamed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Syntax => "xtce.syntax",
            Code::Document => "xtce.document",
            Code::Invalid => "xtce.invalid",
            Code::Reference => "xtce.reference",
            Code::Duplicate => "xtce.duplicate",
            Code::Name => "xtce.name",
            Code::Unsupported => "xtce.unsupported",
            Code::Ignored => "xtce.ignored",
            Code::Renamed => "xtce.renamed",
        }
    }
}

/// What XTCE doesn't say about the bundle.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Options {
    /// The family's name. By default, the first root SpaceSystem's name,
    /// lowercased, with `_` for anything other than letters and digits.
    pub family: Option<String>,
    /// The set's name. By default, the family's.
    pub set: Option<String>,
    /// Where the asset's own id is. By default, a fixed value: the first
    /// root SpaceSystem's name.
    pub alias: Option<Locator>,
}

impl Options {
    pub fn new() -> Self {
        Options::default()
    }

    pub fn with_family(mut self, name: impl Into<String>) -> Self {
        self.family = Some(name.into());
        self
    }

    pub fn with_set(mut self, name: impl Into<String>) -> Self {
        self.set = Some(name.into());
        self
    }

    pub fn with_alias(mut self, alias: Locator) -> Self {
        self.alias = Some(alias);
        self
    }
}

/// Read a bundle from XTCE documents, each holding a root SpaceSystem.
/// References may cross files (`/OtherRoot/Sub/Param`).
///
/// On success, the warnings are what was left out without changing any
/// decoded value. On failure, every problem is reported, warnings
/// included, in file order and then by position.
pub fn read<'a>(
    files: impl IntoIterator<Item = SourceFile<'a>>,
    options: &Options,
) -> Result<Parsed, Vec<SourceDiagnostic>> {
    let files: Vec<SourceFile<'a>> = files.into_iter().collect();
    if files.is_empty() {
        return Err(vec![SourceDiagnostic::error(
            Code::Document.as_str(),
            "there are no XTCE documents to read",
        )]);
    }
    let mut report = Report::new(&files);
    let documents: Vec<_> = files
        .iter()
        .filter_map(|file| xml::parse(*file, &mut report).map(|document| (*file, document)))
        .collect();
    let roots: Vec<El<'_>> = documents
        .iter()
        .map(|(file, document)| El::new(document.root_element(), *file))
        .collect();
    let tree = Tree::build(&roots, &mut report);
    let built = Import::new(&tree, &mut report).bundle(options);
    match built {
        Some((value, map)) if !report.has_errors() => match read_bundle_value(&value) {
            Ok(loaded) => {
                let mut parsed = Parsed::new(loaded.value, map);
                parsed.warnings = report.finish();
                Ok(parsed)
            }
            Err(err) => {
                report.push(map.locate_read_error(&err));
                Err(report.finish())
            }
        },
        _ => Err(report.finish()),
    }
}
