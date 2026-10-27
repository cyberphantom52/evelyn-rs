//! Smali syntax tree. Nodes borrow from the source and remember their byte span, so
//! edits can splice in replacements while leaving everything else byte-identical.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct Class<'a> {
    /// `Lcom/example/Name;`
    pub name: &'a str,
    pub access: Vec<&'a str>,
    pub super_class: Option<&'a str>,
    pub interfaces: Vec<&'a str>,
    pub fields: Vec<Field<'a>>,
    pub methods: Vec<Method<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field<'a> {
    pub span: Range<usize>,
    pub access: Vec<&'a str>,
    pub name: &'a str,
    pub type_desc: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Method<'a> {
    pub span: Range<usize>,
    pub access: Vec<&'a str>,
    pub name: &'a str,
    /// `(Ljava/lang/String;I)Z`
    pub descriptor: &'a str,
    pub body: Vec<Statement<'a>>,
}

impl Method<'_> {
    /// `name(params)return`, the form recipes use to select a method.
    pub fn signature(&self) -> String {
        format!("{}{}", self.name, self.descriptor)
    }

    pub fn is_static(&self) -> bool {
        self.access.contains(&"static")
    }

    /// The return type descriptor, such as `V`, `Z` or `Ljava/lang/Object;`.
    pub fn return_type(&self) -> &str {
        let close = self.descriptor.rfind(')').unwrap_or(0);
        &self.descriptor[close + 1..]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Statement<'a> {
    Instruction(Instruction<'a>),
    Label(&'a str),
    /// `.registers`, `.locals`, `.line`, `.local`, `.catch`, ... with its raw arguments.
    Directive {
        name: &'a str,
        args: &'a str,
    },
    /// A multi-line block kept as source text: annotations, params with
    /// annotations, switch and array payloads.
    Block {
        kind: BlockKind,
        text: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Annotation,
    Param,
    PackedSwitch,
    SparseSwitch,
    ArrayData,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Instruction<'a> {
    pub span: Range<usize>,
    pub opcode: &'a str,
    pub operands: Vec<Operand<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand<'a> {
    Register(&'a str),
    Registers(Vec<&'a str>),
    RegisterRange(&'a str, &'a str),
    Literal(&'a str),
    /// A string literal, quotes and escapes included as written.
    String(&'a str),
    Type(&'a str),
    Label(&'a str),
    Field {
        class: &'a str,
        name: &'a str,
        type_desc: &'a str,
    },
    Method {
        class: &'a str,
        name: &'a str,
        descriptor: &'a str,
    },
    /// Anything else (call sites, method handles), as written.
    Other(&'a str),
}
