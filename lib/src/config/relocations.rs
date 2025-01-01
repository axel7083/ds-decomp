use std::{
    backtrace::Backtrace,
    collections::{btree_map, BTreeMap},
    fmt::Display,
    io::{self, BufRead, BufReader, BufWriter, Write},
    num::ParseIntError,
    ops::Range,
    path::Path,
};

use ds_rom::rom::raw::AutoloadKind;
use snafu::Snafu;

use crate::util::{
    io::{create_file, open_file, FileError},
    parse::{parse_i32, parse_u16, parse_u32},
};

use super::{iter_attributes, module::ModuleKind, ParseContext};

pub struct Relocations {
    relocations: BTreeMap<u32, Relocation>,
}

#[derive(Debug, Snafu)]
pub enum RelocationsParseError {
    #[snafu(transparent)]
    File { source: FileError },
    #[snafu(transparent)]
    Io { source: io::Error },
    #[snafu(transparent)]
    RelocationParse { source: RelocationParseError },
}

#[derive(Debug, Snafu)]
pub enum RelocationsWriteError {
    #[snafu(transparent)]
    File { source: FileError },
    #[snafu(transparent)]
    Io { source: io::Error },
}

#[derive(Debug, Snafu)]
pub enum RelocationsError {
    #[snafu(display("Relocation from {from:#010x} to {curr_to:#010x} in {curr_module} collides with existing one to {prev_to:#010x} in {prev_module}"))]
    RelocationCollision { from: u32, curr_to: u32, curr_module: RelocationModule, prev_to: u32, prev_module: RelocationModule },
}

impl Relocations {
    pub fn new() -> Self {
        Self { relocations: BTreeMap::new() }
    }

    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, RelocationsParseError> {
        let path = path.as_ref();
        let mut context = ParseContext { file_path: path.to_str().unwrap().to_string(), row: 0 };

        let file = open_file(path)?;
        let reader = BufReader::new(file);

        let mut relocations = BTreeMap::new();
        for line in reader.lines() {
            context.row += 1;

            let line = line?;
            let comment_start = line.find("//").unwrap_or(line.len());
            let line = &line[..comment_start];

            let Some(relocation) = Relocation::parse(line, &context)? else {
                continue;
            };
            relocations.insert(relocation.from, relocation);
        }

        Ok(Self { relocations })
    }

    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), RelocationsWriteError> {
        let path = path.as_ref();

        let file = create_file(path)?;
        let mut writer = BufWriter::new(file);

        for relocation in self.relocations.values() {
            writeln!(writer, "{relocation}")?;
        }
        Ok(())
    }

    pub fn add(&mut self, relocation: Relocation) -> Result<&mut Relocation, RelocationsError> {
        match self.relocations.entry(relocation.from) {
            btree_map::Entry::Vacant(entry) => Ok(entry.insert(relocation)),
            btree_map::Entry::Occupied(entry) => {
                if entry.get() == &relocation {
                    log::warn!(
                        "Relocation from {:#010x} to {:#010x} in {} is identical to existing one",
                        relocation.from,
                        relocation.to,
                        relocation.module
                    );
                    Ok(entry.into_mut())
                } else {
                    let other = entry.get();
                    let error = RelocationCollisionSnafu {
                        from: relocation.from,
                        curr_to: relocation.to,
                        curr_module: relocation.module,
                        prev_to: other.to,
                        prev_module: other.module.clone(),
                    }
                    .build();
                    log::error!("{error}");
                    Err(error)
                }
            }
        }
    }

    pub fn add_call(
        &mut self,
        from: u32,
        to: u32,
        module: RelocationModule,
        from_thumb: bool,
        to_thumb: bool,
    ) -> Result<&mut Relocation, RelocationsError> {
        self.add(Relocation::new_call(from, to, module, from_thumb, to_thumb))
    }

    pub fn add_load(
        &mut self,
        from: u32,
        to: u32,
        addend: i32,
        module: RelocationModule,
    ) -> Result<&mut Relocation, RelocationsError> {
        self.add(Relocation::new_load(from, to, addend, module))
    }

    pub fn get(&self, from: u32) -> Option<&Relocation> {
        self.relocations.get(&from)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Relocation> {
        self.relocations.values()
    }

    pub fn iter_range(&self, range: Range<u32>) -> impl Iterator<Item = (&u32, &Relocation)> {
        self.relocations.range(range)
    }
}

#[derive(PartialEq, Eq)]
pub struct Relocation {
    from: u32,
    to: u32,
    addend: i32,
    kind: RelocationKind,
    module: RelocationModule,
    pub source: Option<String>,
}

#[derive(Debug, Snafu)]
pub enum RelocationParseError {
    #[snafu(display("{context}: failed to parse \"from\" address '{value}': {error}\n{backtrace}"))]
    ParseFrom { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display("{context}: failed to parse \"to\" address '{value}': {error}\n{backtrace}"))]
    ParseTo { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display("{context}: failed to parse \"add\" addend '{value}': {error}\n{backtrace}"))]
    ParseAdd { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(transparent)]
    RelocationKindParse { source: RelocationKindParseError },
    #[snafu(transparent)]
    RelocationModuleParse { source: Box<RelocationModuleParseError> },
    #[snafu(display(
        "{context}: expected relocation attribute 'from', 'to', 'add', 'kind' or 'module' but got '{key}':\n{backtrace}"
    ))]
    UnknownAttribute { context: ParseContext, key: String, backtrace: Backtrace },
    #[snafu(display("{context}: missing '{attribute}' attribute"))]
    MissingAttribute { context: ParseContext, attribute: String, backtrace: Backtrace },
}

impl Relocation {
    fn parse(line: &str, context: &ParseContext) -> Result<Option<Self>, RelocationParseError> {
        let words = line.split_whitespace();

        let mut from = None;
        let mut to = None;
        let mut addend = 0;
        let mut kind = None;
        let mut module = None;
        for (key, value) in iter_attributes(words) {
            match key {
                "from" => from = Some(parse_u32(value).map_err(|error| ParseFromSnafu { context, value, error }.build())?),
                "to" => to = Some(parse_u32(value).map_err(|error| ParseToSnafu { context, value, error }.build())?),
                "add" => addend = parse_i32(value).map_err(|error| ParseAddSnafu { context, value, error }.build())?,
                "kind" => kind = Some(RelocationKind::parse(value, context)?),
                "module" => module = Some(RelocationModule::parse(value, context)?),
                _ => return UnknownAttributeSnafu { context, key }.fail(),
            }
        }

        let from = from.ok_or_else(|| MissingAttributeSnafu { context, attribute: "from" }.build())?;
        let to = to.ok_or_else(|| MissingAttributeSnafu { context, attribute: "to" }.build())?;
        let kind = kind.ok_or_else(|| MissingAttributeSnafu { context, attribute: "kind" }.build())?;
        let module = module.ok_or_else(|| MissingAttributeSnafu { context, attribute: "module" }.build())?;

        Ok(Some(Self { from, to, addend, kind, module, source: None }))
    }

    pub fn new_call(from: u32, to: u32, module: RelocationModule, from_thumb: bool, to_thumb: bool) -> Self {
        Self {
            from,
            to,
            addend: 0,
            kind: match (from_thumb, to_thumb) {
                (true, true) => RelocationKind::ThumbCall,
                (true, false) => RelocationKind::ThumbCallArm,
                (false, true) => RelocationKind::ArmCallThumb,
                (false, false) => RelocationKind::ArmCall,
            },
            module,
            source: None,
        }
    }

    pub fn new_branch(from: u32, to: u32, module: RelocationModule) -> Self {
        Self { from, to, addend: 0, kind: RelocationKind::ArmBranch, module, source: None }
    }

    pub fn new_load(from: u32, to: u32, addend: i32, module: RelocationModule) -> Self {
        Self { from, to, addend, kind: RelocationKind::Load, module, source: None }
    }

    pub fn from_address(&self) -> u32 {
        self.from
    }

    pub fn to_address(&self) -> u32 {
        self.to
    }

    pub fn kind(&self) -> RelocationKind {
        self.kind
    }

    pub fn module(&self) -> &RelocationModule {
        &self.module
    }

    pub fn addend(&self) -> i64 {
        self.addend as i64 + self.kind.addend()
    }

    pub fn addend_value(&self) -> i32 {
        self.addend
    }
}

impl Display for Relocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "from:{:#010x} kind:{} to:{:#010x} module:{}", self.from, self.kind, self.to, self.module)?;
        if let Some(source) = &self.source {
            write!(f, " // {source}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RelocationKind {
    ArmCall,
    ThumbCall,
    ArmCallThumb,
    ThumbCallArm,
    ArmBranch,
    Load,
}

#[derive(Debug, Snafu)]
pub enum RelocationKindParseError {
    #[snafu(display("{context}: unknown relocation kind '{value}', must be one of: arm_call, thumb_call, arm_call_thumb, thumb_call_arm, arm_branch, load:\n{backtrace}"))]
    UnknownKind { context: ParseContext, value: String, backtrace: Backtrace },
}

impl RelocationKind {
    fn parse(value: &str, context: &ParseContext) -> Result<Self, RelocationKindParseError> {
        match value {
            "arm_call" => Ok(Self::ArmCall),
            "thumb_call" => Ok(Self::ThumbCall),
            "arm_call_thumb" => Ok(Self::ArmCallThumb),
            "thumb_call_arm" => Ok(Self::ThumbCallArm),
            "arm_branch" => Ok(Self::ArmBranch),
            "load" => Ok(Self::Load),
            _ => UnknownKindSnafu { context, value }.fail(),
        }
    }

    pub fn addend(&self) -> i64 {
        match self {
            Self::ArmCall => -8,
            Self::ThumbCall => -4,
            Self::ArmCallThumb => -8,
            Self::ThumbCallArm => -4,
            Self::ArmBranch => -8,
            Self::Load => 0,
        }
    }
}

impl Display for RelocationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArmCall => write!(f, "arm_call"),
            Self::ThumbCall => write!(f, "thumb_call"),
            Self::ArmCallThumb => write!(f, "arm_call_thumb"),
            Self::ThumbCallArm => write!(f, "thumb_call_arm"),
            Self::ArmBranch => write!(f, "arm_branch"),
            Self::Load => write!(f, "load"),
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum RelocationModule {
    None,
    Overlay { id: u16 },
    Overlays { ids: Vec<u16> },
    Main,
    Itcm,
    Dtcm,
}

#[derive(Debug, Snafu)]
pub enum RelocationModuleParseError {
    #[snafu(display("{context}: relocations to '{module}' have no options, but got '({options})':\n{backtrace}"))]
    UnexpectedOptions { context: ParseContext, module: String, options: String, backtrace: Backtrace },
    #[snafu(display("{context}: failed to parse overlay ID '{value}': {error}\n{backtrace}"))]
    ParseOverlayId { context: ParseContext, value: String, error: ParseIntError, backtrace: Backtrace },
    #[snafu(display("{context}: relocation to 'overlays' must have two or more overlay IDs, but got {ids:?}:\n{backtrace}"))]
    ExpectedMultipleOverlays { context: ParseContext, ids: Vec<u16>, backtrace: Backtrace },
    #[snafu(display(
        "{context}: unknown relocation to '{module}', must be one of: overlays, overlay, main, itcm, dtcm, none:\n{backtrace}"
    ))]
    UnknownModule { context: ParseContext, module: String, backtrace: Backtrace },
}

#[derive(Debug, Snafu)]
pub enum RelocationModuleKindNotSupportedError {
    #[snafu(display("Unknown autoload kind '{kind}'"))]
    UnknownAutoload { kind: AutoloadKind, backtrace: Backtrace },
}

impl RelocationModule {
    fn parse(text: &str, context: &ParseContext) -> Result<Self, Box<RelocationModuleParseError>> {
        let (value, options) = text.split_once('(').unwrap_or((text, ""));
        let options = options.strip_suffix(')').unwrap_or(options);

        match value {
            "none" => {
                if options.is_empty() {
                    Ok(Self::None)
                } else {
                    Err(Box::new(UnexpectedOptionsSnafu { context, module: "none", options }.build()))
                }
            }
            "overlay" => Ok(Self::Overlay {
                id: parse_u16(options).map_err(|error| ParseOverlayIdSnafu { context, value: options, error }.build())?,
            }),
            "overlays" => {
                let ids = options
                    .split(',')
                    .map(|x| parse_u16(x).map_err(|error| ParseOverlayIdSnafu { context, value: x, error }.build()))
                    .collect::<Result<Vec<_>, _>>()?;
                if ids.len() < 2 {
                    Err(Box::new(ExpectedMultipleOverlaysSnafu { context, ids }.build()))
                } else {
                    Ok(Self::Overlays { ids })
                }
            }
            "main" => {
                if options.is_empty() {
                    Ok(Self::Main)
                } else {
                    Err(Box::new(UnexpectedOptionsSnafu { context, module: "main", options }.build()))
                }
            }
            "itcm" => {
                if options.is_empty() {
                    Ok(Self::Itcm)
                } else {
                    Err(Box::new(UnexpectedOptionsSnafu { context, module: "itcm", options }.build()))
                }
            }
            "dtcm" => {
                if options.is_empty() {
                    Ok(Self::Dtcm)
                } else {
                    Err(Box::new(UnexpectedOptionsSnafu { context, module: "dtcm", options }.build()))
                }
            }
            _ => Err(Box::new(UnknownModuleSnafu { context, module: value }.build())),
        }
    }
}

impl TryFrom<ModuleKind> for RelocationModule {
    type Error = RelocationModuleKindNotSupportedError;

    fn try_from(value: ModuleKind) -> Result<Self, Self::Error> {
        match value {
            ModuleKind::Arm9 => Ok(Self::Main),
            ModuleKind::Overlay(id) => Ok(Self::Overlay { id }),
            ModuleKind::Autoload(kind) => match kind {
                AutoloadKind::Itcm => Ok(Self::Itcm),
                AutoloadKind::Dtcm => Ok(Self::Dtcm),
                AutoloadKind::Unknown(_) => UnknownAutoloadSnafu { kind }.fail(),
            },
        }
    }
}

impl Display for RelocationModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelocationModule::None => write!(f, "none"),
            RelocationModule::Overlay { id } => write!(f, "overlay({id})"),
            RelocationModule::Overlays { ids } => {
                write!(f, "overlays({}", ids[0])?;
                for id in &ids[1..] {
                    write!(f, ",{}", id)?;
                }
                write!(f, ")")?;
                Ok(())
            }
            RelocationModule::Main => write!(f, "main"),
            RelocationModule::Itcm => write!(f, "itcm"),
            RelocationModule::Dtcm => write!(f, "dtcm"),
        }
    }
}
