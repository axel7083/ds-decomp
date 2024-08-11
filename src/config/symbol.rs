use anyhow::{bail, Context, Result};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};
use unarm::LookupSymbol;

use crate::{
    analysis::{functions::Function, jump_table::JumpTable},
    util::{
        io::{create_file, open_file},
        parse::parse_u32,
    },
};

use super::{iter_attributes, ParseContext};

type SymbolIndex = usize;

pub struct SymbolMap {
    symbols: Vec<Symbol>,
    symbols_by_address: BTreeMap<u32, Vec<SymbolIndex>>,
    symbols_by_name: HashMap<String, Vec<SymbolIndex>>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self::from_symbols(vec![])
    }

    pub fn from_symbols(symbols: Vec<Symbol>) -> Self {
        let mut symbols_by_address = BTreeMap::<u32, Vec<_>>::new();
        let mut symbols_by_name = HashMap::<String, Vec<_>>::new();

        for (index, symbol) in symbols.iter().enumerate() {
            symbols_by_address.entry(symbol.addr).or_default().push(index);
            symbols_by_name.entry(symbol.name.clone()).or_default().push(index);
        }

        Self { symbols, symbols_by_address, symbols_by_name }
    }

    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut context = ParseContext { file_path: path.to_str().unwrap().to_string(), row: 0 };

        let file = open_file(path)?;
        let reader = BufReader::new(file);

        let mut symbol_map = Self::from_symbols(vec![]);
        for line in reader.lines() {
            context.row += 1;
            let Some(symbol) = Symbol::parse(line?.as_str(), &context)? else { continue };
            symbol_map.add(symbol)?;
        }
        Ok(symbol_map)
    }

    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        let file = create_file(path)?;
        let mut writer = BufWriter::new(file);

        for indices in self.symbols_by_address.values() {
            for &index in indices {
                let symbol = &self.symbols[index];
                if symbol.should_write() {
                    writeln!(writer, "{symbol}")?;
                }
            }
        }

        Ok(())
    }

    pub fn for_address(&self, address: u32) -> Option<impl DoubleEndedIterator<Item = (SymbolIndex, &Symbol)>> {
        Some(self.symbols_by_address.get(&address)?.iter().map(|&i| (i, &self.symbols[i])))
    }

    pub fn by_address(&self, address: u32) -> Option<(SymbolIndex, &Symbol)> {
        let Some(mut symbols) = self.for_address(address) else {
            return None;
        };
        let (index, symbol) = symbols.next().unwrap();
        if let Some((_, other)) = symbols.next() {
            panic!("multiple symbols at 0x{:08x}: {}, {}", address, symbol.name, other.name);
        }
        Some((index, symbol))
    }

    pub fn for_name(&self, name: &str) -> Option<impl DoubleEndedIterator<Item = (SymbolIndex, &Symbol)>> {
        Some(self.symbols_by_name.get(name)?.iter().map(|&i| (i, &self.symbols[i])))
    }

    pub fn by_name(&self, name: &str) -> Result<Option<(SymbolIndex, &Symbol)>> {
        let Some(mut symbols) = self.for_name(name) else {
            return Ok(None);
        };
        let (index, symbol) = symbols.next().unwrap();
        if let Some((_, other)) = symbols.next() {
            bail!("multiple symbols with name '{}': 0x{:08x}, 0x{:08x}", name, symbol.addr, other.addr);
        }
        Ok(Some((index, symbol)))
    }

    pub fn add(&mut self, symbol: Symbol) -> Result<()> {
        let index = self.symbols.len();
        self.symbols_by_address.entry(symbol.addr).or_default().push(index);
        self.symbols_by_name.entry(symbol.name.clone()).or_default().push(index);
        self.symbols.push(symbol);

        Ok(())
    }

    pub fn add_if_new_address(&mut self, symbol: Symbol) -> Result<()> {
        if self.symbols_by_address.contains_key(&symbol.addr) {
            Ok(())
        } else {
            self.add(symbol)
        }
    }

    pub fn add_function(&mut self, function: &Function) -> Result<()> {
        self.add(Symbol::from_function(function))
    }

    fn label_name(addr: u32) -> String {
        format!("_{:08x}", addr)
    }

    pub fn add_label(&mut self, addr: u32) -> Result<()> {
        let name = Self::label_name(addr);
        self.add_if_new_address(Symbol::new_label(name, addr))
    }

    pub fn get_label(&self, addr: u32) -> Option<&Symbol> {
        self.by_address(addr).map_or(None, |(_, s)| (s.kind == SymbolKind::Label).then_some(s))
    }

    pub fn add_pool_constant(&mut self, addr: u32) -> Result<()> {
        let name = Self::label_name(addr);
        self.add_if_new_address(Symbol::new_pool_constant(name, addr))
    }

    pub fn get_pool_constant(&self, addr: u32) -> Option<&Symbol> {
        self.by_address(addr).map_or(None, |(_, s)| (s.kind == SymbolKind::PoolConstant).then_some(s))
    }

    pub fn add_jump_table(&mut self, table: &JumpTable) -> Result<()> {
        let name = Self::label_name(table.address);
        self.add(Symbol::new_jump_table(name, table.address, table.size, table.code))
    }

    pub fn get_jump_table(&self, addr: u32) -> Option<(SymJumpTable, &Symbol)> {
        self.by_address(addr).map_or(None, |(_, s)| match s.kind {
            SymbolKind::JumpTable(jump_table) => Some((jump_table, s)),
            _ => None,
        })
    }

    pub fn add_data(&mut self, name: Option<String>, addr: u32, data: SymData) -> Result<()> {
        let name = name.unwrap_or_else(|| Self::label_name(addr));
        self.add(Symbol::new_data(name, addr, data))
    }

    pub fn get_data(&self, addr: u32) -> Option<(SymData, &Symbol)> {
        self.by_address(addr).map_or(None, |(_, s)| match s.kind {
            SymbolKind::Data(data) => Some((data, s)),
            _ => None,
        })
    }
}

impl LookupSymbol for SymbolMap {
    fn lookup_symbol_name(&self, _source: u32, destination: u32) -> Option<&str> {
        let Some((_, symbol)) = self.by_address(destination) else {
            return None;
        };
        Some(&symbol.name)
    }
}

#[derive(Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub addr: u32,
}

impl Symbol {
    pub fn parse(line: &str, context: &ParseContext) -> Result<Option<Self>> {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { return Ok(None) };

        let mut kind = None;
        let mut addr = None;
        for pair in iter_attributes(words, context) {
            let (key, value) = pair?;
            match key {
                "kind" => kind = Some(SymbolKind::parse(value, context)?),
                "addr" => {
                    addr = Some(parse_u32(value).with_context(|| format!("{}: failed to parse address '{}'", context, value))?)
                }
                _ => bail!("{}: expected symbol attribute 'kind' or 'addr' but got '{}'", context, key),
            }
        }

        let name = name.to_string().into();
        let kind = kind.with_context(|| format!("{}: missing 'kind' attribute", context))?;
        let addr = addr.with_context(|| format!("{}: missing 'addr' attribute", context))?;

        Ok(Some(Symbol { name, kind, addr }))
    }

    fn should_write(&self) -> bool {
        self.kind.should_write()
    }

    pub fn from_function(function: &Function) -> Self {
        Self {
            name: function.name().to_string(),
            kind: SymbolKind::Function(SymFunction { mode: InstructionMode::from_thumb(function.is_thumb()) }),
            addr: function.start_address(),
        }
    }

    pub fn new_label(name: String, addr: u32) -> Self {
        Self { name, kind: SymbolKind::Label, addr }
    }

    pub fn new_pool_constant(name: String, addr: u32) -> Self {
        Self { name, kind: SymbolKind::PoolConstant, addr }
    }

    pub fn new_jump_table(name: String, addr: u32, size: u32, code: bool) -> Self {
        Self { name, kind: SymbolKind::JumpTable(SymJumpTable { size, code }), addr }
    }

    fn new_data(name: String, addr: u32, data: SymData) -> Symbol {
        Self { name, kind: SymbolKind::Data(data), addr }
    }
}

impl Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} kind:{} addr:{:#x}", self.name, self.kind, self.addr)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function(SymFunction),
    Label,
    PoolConstant,
    JumpTable(SymJumpTable),
    Data(SymData),
    Bss,
}

impl SymbolKind {
    pub fn parse(text: &str, context: &ParseContext) -> Result<Self> {
        let (kind, options) = text.split_once('(').unwrap_or((text, ""));
        let options = options.strip_suffix(')').unwrap_or(options);

        match kind {
            "function" => Ok(Self::Function(SymFunction::parse(options, context)?)),
            "data" => Ok(Self::Data(SymData::parse(options, context)?)),
            "bss" => Ok(Self::Bss),
            _ => bail!("{}: unknown symbol kind '{}', must be one of: function, data, bss", context, kind),
        }
    }

    fn should_write(&self) -> bool {
        matches!(self, SymbolKind::Function(_) | SymbolKind::Data(_) | SymbolKind::Bss)
    }
}

impl Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolKind::Function(function) => write!(f, "function({function})")?,
            SymbolKind::Data(data) => write!(f, "data({data})")?,
            SymbolKind::Bss => write!(f, "bss")?,
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SymFunction {
    pub mode: InstructionMode,
}

impl SymFunction {
    pub fn parse(options: &str, context: &ParseContext) -> Result<Self> {
        let mode = InstructionMode::parse(options, context)?;
        Ok(Self { mode })
    }
}

impl Display for SymFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.mode)
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum InstructionMode {
    #[default]
    Auto,
    Arm,
    Thumb,
}

impl InstructionMode {
    pub fn parse(text: &str, context: &ParseContext) -> Result<Self> {
        match text {
            "" | "auto" => Ok(Self::Auto),
            "arm" => Ok(Self::Arm),
            "thumb" => Ok(Self::Thumb),
            _ => bail!("{}: expected instruction mode 'auto', 'arm' or 'thumb' but got '{}'", context, text),
        }
    }

    pub fn write(self, writer: &mut BufWriter<File>) -> Result<()> {
        match self {
            InstructionMode::Auto => write!(writer, "auto")?,
            InstructionMode::Arm => write!(writer, "arm")?,
            InstructionMode::Thumb => write!(writer, "thumb")?,
        }
        Ok(())
    }

    pub fn from_thumb(thumb: bool) -> Self {
        if thumb {
            Self::Thumb
        } else {
            Self::Arm
        }
    }
}

impl Display for InstructionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Arm => write!(f, "arm"),
            Self::Thumb => write!(f, "thumb"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SymJumpTable {
    pub size: u32,
    pub code: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SymData {
    pub kind: DataKind,
    pub count: u32,
}

impl SymData {
    pub fn parse(options: &str, context: &ParseContext) -> Result<Self> {
        let mut kind = DataKind::Word;
        let mut count = 1;
        for option in options.split(',') {
            if let Some((key, value)) = option.split_once('=') {
                match key {
                    "count" => count = parse_u32(value)?,
                    _ => bail!("{context}: expected data type or 'count=...' but got '{key}={value}'"),
                }
            } else {
                kind = DataKind::parse(option, context)?;
            }
        }

        Ok(Self { kind, count })
    }

    pub fn size(&self) -> usize {
        self.kind.size() * self.count as usize
    }
}

impl Display for SymData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)?;
        if self.count != 1 {
            write!(f, ",count={}", self.count)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    Byte,
    Word,
}

impl DataKind {
    pub fn parse(text: &str, context: &ParseContext) -> Result<Self> {
        match text {
            "byte" => Ok(Self::Byte),
            "word" => Ok(Self::Word),
            _ => bail!("{context}: expected data kind 'byte' or 'word' but got '{text}'"),
        }
    }

    pub fn size(&self) -> usize {
        match self {
            DataKind::Byte => 1,
            DataKind::Word => 4,
        }
    }

    pub fn write_directive(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataKind::Byte => write!(f, ".byte"),
            DataKind::Word => write!(f, ".word"),
        }
    }

    pub fn write_raw(&self, data: &[u8], f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if data.len() < self.size() as usize {
            panic!("not enough bytes to write raw data directive");
        }
        match self {
            DataKind::Byte => write!(f, "0x{:02x}", data[0]),
            DataKind::Word => write!(f, "0x{:08x}", u32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        }
    }
}

impl Display for DataKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataKind::Byte => write!(f, "byte"),
            DataKind::Word => write!(f, "word"),
        }
    }
}
