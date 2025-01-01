use std::{
    backtrace::Backtrace,
    collections::{BTreeMap, HashMap},
    fmt::Display,
    num::ParseIntError,
    ops::Range,
};

use snafu::Snafu;

use crate::{
    analysis::functions::Function,
    util::{bytes::FromSlice, parse::parse_u32},
};

use super::{iter_attributes, module::Module, ParseContext};

#[derive(Clone, Copy)]
pub struct SectionIndex(pub usize);

#[derive(Clone)]
pub struct Section {
    name: String,
    kind: SectionKind,
    start_address: u32,
    end_address: u32,
    alignment: u32,
    functions: BTreeMap<u32, Function>,
}

#[derive(Debug, Snafu)]
pub enum SectionError {
    #[snafu(display(
        "Section {name} must not end ({end_address:#010x}) before it starts ({start_address:#010x}):\n{backtrace}"
    ))]
    EndBeforeStart { name: String, start_address: u32, end_address: u32, backtrace: Backtrace },
    #[snafu(display("Section {name} aligment ({alignment}) must be a power of two:\n{backtrace}"))]
    AlignmentPowerOfTwo { name: String, alignment: u32, backtrace: Backtrace },
    #[snafu(display("Section {name} starts at a misaligned address {start_address:#010x}; the provided alignment was {alignment}:\n{backtrace}"))]
    MisalignedStart { name: String, start_address: u32, alignment: u32, backtrace: Backtrace },
}

#[derive(Debug, Snafu)]
pub enum SectionParseError {
    #[snafu(transparent)]
    SectionKind { source: SectionKindError },
    #[snafu(display("{context}: failed to parse start address '{value}': {error}\n{backtrace}"))]
    ParseStartAddress { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display("{context}: failed to parse end address '{value}': {error}\n{backtrace}"))]
    ParseEndAddress { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display("{context}: failed to parse alignment '{value}': {error}\n{backtrace}"))]
    ParseAlignment { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display(
        "{context}: expected section attribute 'kind', 'start', 'end' or 'align' but got '{key}':\n{backtrace}"
    ))]
    UnknownAttribute { context: ParseContext, key: String, backtrace: Backtrace },
    #[snafu(display("{context}: missing '{attribute}' attribute:\n{backtrace}"))]
    MissingAttribute { context: ParseContext, attribute: String, backtrace: Backtrace },
    #[snafu(display("{context}: {error}"))]
    Section { context: ParseContext, error: SectionError },
}

#[derive(Debug, Snafu)]
pub enum SectionInheritParseError {
    #[snafu(display("{context}: section {name} does not exist in this file's header:\n{backtrace}"))]
    NotInHeader { context: ParseContext, name: String, backtrace: Backtrace },
    #[snafu(display("{context}: attribute '{attribute}' should be omitted as it is inherited from this file's header"))]
    InheritedAttribute { context: ParseContext, attribute: String, backtrace: Backtrace },
    #[snafu(transparent)]
    SectionParse { source: SectionParseError },
}

#[derive(Debug, Snafu)]
pub enum SectionCodeError {
    #[snafu(display("section starts before base address:\n{backtrace}"))]
    StartsBeforeBaseAddress { backtrace: Backtrace },
    #[snafu(display("section ends after code ends:\n{backtrace}"))]
    EndsOutsideModule { backtrace: Backtrace },
}

impl Section {
    pub fn new(
        name: String,
        kind: SectionKind,
        start_address: u32,
        end_address: u32,
        alignment: u32,
    ) -> Result<Self, SectionError> {
        Self::with_functions(name, kind, start_address, end_address, alignment, BTreeMap::new())
    }

    pub fn with_functions(
        name: String,
        kind: SectionKind,
        start_address: u32,
        end_address: u32,
        alignment: u32,
        functions: BTreeMap<u32, Function>,
    ) -> Result<Self, SectionError> {
        if end_address < start_address {
            return EndBeforeStartSnafu { name, start_address, end_address }.fail();
        }
        if !alignment.is_power_of_two() {
            return AlignmentPowerOfTwoSnafu { name, alignment }.fail();
        }
        let misalign_mask = alignment - 1;
        if (start_address & misalign_mask) != 0 {
            return MisalignedStartSnafu { name, start_address, alignment }.fail();
        }

        Ok(Self { name, kind, start_address, end_address, alignment, functions })
    }

    pub fn inherit(other: &Section, start_address: u32, end_address: u32) -> Result<Self, SectionError> {
        if end_address < start_address {
            return EndBeforeStartSnafu { name: other.name.clone(), start_address, end_address }.fail();
        }
        Ok(Self {
            name: other.name.clone(),
            kind: other.kind,
            start_address,
            end_address,
            alignment: other.alignment,
            functions: BTreeMap::new(),
        })
    }

    pub(crate) fn parse(line: &str, context: &ParseContext) -> Result<Option<Self>, SectionParseError> {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { return Ok(None) };

        let mut kind = None;
        let mut start = None;
        let mut end = None;
        let mut align = None;
        for (key, value) in iter_attributes(words) {
            match key {
                "kind" => kind = Some(SectionKind::parse(value, context)?),
                "start" => {
                    start = Some(parse_u32(value).map_err(|error| ParseStartAddressSnafu { context, value, error }.build())?)
                }
                "end" => end = Some(parse_u32(value).map_err(|error| ParseEndAddressSnafu { context, value, error }.build())?),
                "align" => {
                    align = Some(parse_u32(value).map_err(|error| ParseAlignmentSnafu { context, value, error }.build())?)
                }
                _ => return UnknownAttributeSnafu { context: context.clone(), key }.fail(),
            }
        }

        Ok(Some(
            Section::new(
                name.to_string(),
                kind.ok_or_else(|| MissingAttributeSnafu { context, attribute: "kind" }.build())?,
                start.ok_or_else(|| MissingAttributeSnafu { context, attribute: "start" }.build())?,
                end.ok_or_else(|| MissingAttributeSnafu { context, attribute: "end" }.build())?,
                align.ok_or_else(|| MissingAttributeSnafu { context, attribute: "align" }.build())?,
            )
            .map_err(|error| SectionSnafu { context, error }.build())?,
        ))
    }

    pub(crate) fn parse_inherit(
        line: &str,
        context: &ParseContext,
        sections: &Sections,
    ) -> Result<Option<Self>, SectionInheritParseError> {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { return Ok(None) };

        let (_, inherit_section) = sections.by_name(name).ok_or_else(|| NotInHeaderSnafu { context, name }.build())?;

        let mut start = None;
        let mut end = None;
        for (key, value) in iter_attributes(words) {
            match key {
                "kind" => return InheritedAttributeSnafu { context, attribute: "kind" }.fail(),
                "start" => {
                    start = Some(parse_u32(value).map_err(|error| ParseStartAddressSnafu { context, value, error }.build())?)
                }
                "end" => end = Some(parse_u32(value).map_err(|error| ParseEndAddressSnafu { context, value, error }.build())?),
                "align" => return InheritedAttributeSnafu { context, attribute: "align" }.fail(),
                _ => return UnknownAttributeSnafu { context, key }.fail()?,
            }
        }

        Ok(Some(
            Section::inherit(
                inherit_section,
                start.ok_or_else(|| MissingAttributeSnafu { context, attribute: "start" }.build())?,
                end.ok_or_else(|| MissingAttributeSnafu { context, attribute: "end" }.build())?,
            )
            .map_err(|error| SectionSnafu { context, error }.build())?,
        ))
    }

    pub fn code_from_module<'a>(&'a self, module: &'a Module) -> Result<Option<&'a [u8]>, SectionCodeError> {
        self.code(module.code(), module.base_address())
    }

    pub fn code<'a>(&'a self, code: &'a [u8], base_address: u32) -> Result<Option<&'a [u8]>, SectionCodeError> {
        if self.kind() == SectionKind::Bss {
            return Ok(None);
        }
        if self.start_address() < base_address {
            return StartsBeforeBaseAddressSnafu.fail();
        }
        let start = self.start_address() - base_address;
        let end = self.end_address() - base_address;
        if end > code.len() as u32 {
            return EndsOutsideModuleSnafu.fail();
        }
        Ok(Some(&code[start as usize..end as usize]))
    }

    pub fn size(&self) -> u32 {
        self.end_address - self.start_address
    }

    /// Iterates over every 32-bit word in the specified `range`, which defaults to the entire section if it is `None`. Note
    /// that `code` must be the full raw content of this section.
    pub fn iter_words<'a>(&'a self, code: &'a [u8], range: Option<Range<u32>>) -> impl Iterator<Item = Word> + 'a {
        let range = range.unwrap_or(self.address_range());
        let start = range.start.next_multiple_of(4);
        let end = range.end & !3;

        (start..end).step_by(4).map(move |address| {
            let offset = address - self.start_address();
            let bytes = &code[offset as usize..];
            Word { address, value: u32::from_le_slice(bytes) }
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> SectionKind {
        self.kind
    }

    pub fn start_address(&self) -> u32 {
        self.start_address
    }

    pub fn end_address(&self) -> u32 {
        self.end_address
    }

    pub fn address_range(&self) -> Range<u32> {
        self.start_address..self.end_address
    }

    pub fn alignment(&self) -> u32 {
        self.alignment
    }

    pub fn overlaps_with(&self, other: &Section) -> bool {
        self.start_address < other.end_address && other.start_address < self.end_address
    }

    pub fn functions(&self) -> &BTreeMap<u32, Function> {
        &self.functions
    }

    pub fn functions_mut(&mut self) -> &mut BTreeMap<u32, Function> {
        &mut self.functions
    }

    pub fn add_function(&mut self, function: Function) {
        self.functions.insert(function.start_address(), function);
    }
}

impl Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:11} start:{:#010x} end:{:#010x} kind:{} align:{}",
            self.name, self.start_address, self.end_address, self.kind, self.alignment
        )
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SectionKind {
    Code,
    Data,
    Bss,
}

#[derive(Debug, Snafu)]
pub enum SectionKindError {
    #[snafu(display("{context}: unknown section kind '{value}', must be one of: code, data, bss"))]
    UnknownKind { context: ParseContext, value: String, backtrace: Backtrace },
}

impl SectionKind {
    pub fn parse(value: &str, context: &ParseContext) -> Result<Self, SectionKindError> {
        match value {
            "code" => Ok(Self::Code),
            "data" => Ok(Self::Data),
            "bss" => Ok(Self::Bss),
            _ => UnknownKindSnafu { context, value }.fail(),
        }
    }

    pub fn is_initialized(self) -> bool {
        match self {
            SectionKind::Code => true,
            SectionKind::Data => true,
            SectionKind::Bss => false,
        }
    }
}

impl Display for SectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code => write!(f, "code"),
            Self::Data => write!(f, "data"),
            Self::Bss => write!(f, "bss"),
        }
    }
}

pub struct Sections {
    sections: Vec<Section>,
    sections_by_name: HashMap<String, SectionIndex>,
}

#[derive(Debug, Snafu)]
pub enum SectionsError {
    #[snafu(display("Section '{name}' already exists:\n{backtrace}"))]
    DuplicateName { name: String, backtrace: Backtrace },
    #[snafu(display("Section '{name}' overlaps with '{other_name}':\n{backtrace}"))]
    Overlapping { name: String, other_name: String, backtrace: Backtrace },
}

impl Sections {
    pub fn new() -> Self {
        Self { sections: vec![], sections_by_name: HashMap::new() }
    }

    pub fn add(&mut self, section: Section) -> Result<SectionIndex, SectionsError> {
        if self.sections_by_name.contains_key(&section.name) {
            return DuplicateNameSnafu { name: section.name }.fail();
        }
        for other in &self.sections {
            if section.overlaps_with(other) {
                return OverlappingSnafu { name: section.name, other_name: other.name.to_string() }.fail();
            }
        }

        let index = SectionIndex(self.sections.len());
        self.sections_by_name.insert(section.name.clone(), index);
        self.sections.push(section);
        Ok(index)
    }

    pub fn get(&self, index: usize) -> &Section {
        &self.sections[index]
    }

    pub fn get_mut(&mut self, index: usize) -> &mut Section {
        &mut self.sections[index]
    }

    pub fn by_name(&self, name: &str) -> Option<(SectionIndex, &Section)> {
        let &index = self.sections_by_name.get(name)?;
        Some((index, &self.sections[index.0]))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Section> {
        self.sections.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn get_by_contained_address(&self, address: u32) -> Option<(SectionIndex, &Section)> {
        self.sections
            .iter()
            .enumerate()
            .find(|(_, s)| address >= s.start_address && address < s.end_address)
            .map(|(i, s)| (SectionIndex(i), s))
    }

    pub fn get_by_contained_address_mut(&mut self, address: u32) -> Option<(SectionIndex, &mut Section)> {
        self.sections
            .iter_mut()
            .enumerate()
            .find(|(_, s)| address >= s.start_address && address < s.end_address)
            .map(|(i, s)| (SectionIndex(i), s))
    }

    pub fn add_function(&mut self, function: Function) {
        let address = function.first_instruction_address();
        self.sections
            .iter_mut()
            .find(|s| address >= s.start_address && address < s.end_address)
            .unwrap()
            .functions
            .insert(address, function);
    }

    pub fn sorted_by_address(&self) -> Vec<&Section> {
        let mut sections = self.sections.iter().collect::<Vec<_>>();
        sections.sort_unstable_by_key(|s| s.start_address);
        sections
    }

    pub fn functions(&self) -> impl Iterator<Item = &Function> {
        self.sections.iter().flat_map(|s| s.functions.values())
    }

    pub fn functions_mut(&mut self) -> impl Iterator<Item = &mut Function> {
        self.sections.iter_mut().flat_map(|s| s.functions.values_mut())
    }

    pub fn base_address(&self) -> Option<u32> {
        self.sections.iter().map(|s| s.start_address).min()
    }

    pub fn end_address(&self) -> Option<u32> {
        self.sections.iter().map(|s| s.end_address).max()
    }

    pub fn bss_size(&self) -> u32 {
        self.sections.iter().filter(|s| s.kind == SectionKind::Bss).map(|s| s.size()).sum()
    }

    pub fn bss_range(&self) -> Option<Range<u32>> {
        self.sections
            .iter()
            .filter(|s| s.kind == SectionKind::Bss)
            .map(|s| s.address_range())
            .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
    }
}

impl IntoIterator for Sections {
    type Item = Section;

    type IntoIter = <Vec<Self::Item> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.sections.into_iter()
    }
}

pub struct Word {
    pub address: u32,
    pub value: u32,
}
