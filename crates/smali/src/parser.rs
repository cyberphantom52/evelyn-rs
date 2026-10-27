use crate::ast::{BlockKind, Class, Field, Instruction, Method, Operand, Statement};
use pest::{Parser, iterators::Pair};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "src/smali.pest"]
struct SmaliParser;

pub type Error = Box<pest::error::Error<Rule>>;

pub fn parse(source: &str) -> Result<Class<'_>, Error> {
    let file = SmaliParser::parse(Rule::file, source)?
        .next()
        .expect("file rule");
    let mut class = Class {
        name: "",
        access: Vec::new(),
        super_class: None,
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
    };
    for item in file.into_inner() {
        match item.as_rule() {
            Rule::class => {
                let (access, rest) = split_access(item);
                class.access = access;
                class.name = rest[0].as_str();
            }
            Rule::super_class => class.super_class = Some(first(item)),
            Rule::implements => class.interfaces.push(first(item)),
            Rule::field => class.fields.push(field(item)),
            Rule::method => class.methods.push(method(item)),
            _ => {}
        }
    }
    Ok(class)
}

fn first<'a>(pair: Pair<'a, Rule>) -> &'a str {
    pair.into_inner().next().map_or("", |p| p.as_str())
}

/// Leading access flags, then the remaining children.
fn split_access(pair: Pair<'_, Rule>) -> (Vec<&str>, Vec<Pair<'_, Rule>>) {
    let (access, rest): (Vec<_>, Vec<_>) =
        pair.into_inner().partition(|p| p.as_rule() == Rule::access);
    (access.iter().map(|p| p.as_str()).collect(), rest)
}

fn field(pair: Pair<'_, Rule>) -> Field<'_> {
    let span = pair.as_span();
    let (access, rest) = split_access(pair);
    Field {
        span: span.start()..span.end(),
        access,
        name: rest[0].as_str(),
        type_desc: rest[1].as_str(),
    }
}

fn method(pair: Pair<'_, Rule>) -> Method<'_> {
    let span = pair.as_span();
    let (access, rest) = split_access(pair);
    let mut rest = rest.into_iter();
    let name = rest.next().map_or("", |p| p.as_str());
    let descriptor = rest.next().map_or("", |p| p.as_str());
    Method {
        span: span.start()..span.end(),
        access,
        name,
        descriptor,
        body: rest.filter_map(statement).collect(),
    }
}

fn statement(pair: Pair<'_, Rule>) -> Option<Statement<'_>> {
    let block = |kind| Statement::Block {
        kind,
        text: pair.as_str(),
    };
    Some(match pair.as_rule() {
        Rule::label_def => Statement::Label(first(pair)),
        Rule::annotation => block(BlockKind::Annotation),
        Rule::param => block(BlockKind::Param),
        Rule::packed_switch => block(BlockKind::PackedSwitch),
        Rule::sparse_switch => block(BlockKind::SparseSwitch),
        Rule::array_data => block(BlockKind::ArrayData),
        Rule::directive => {
            let mut inner = pair.into_inner();
            Statement::Directive {
                name: inner.next().map_or("", |p| p.as_str()),
                args: inner.next().map_or("", |p| p.as_str().trim()),
            }
        }
        Rule::instruction => {
            let span = pair.as_span();
            let mut inner = pair.into_inner();
            Statement::Instruction(Instruction {
                span: span.start()..span.end(),
                opcode: inner.next().map_or("", |p| p.as_str()),
                operands: inner.map(operand).collect(),
            })
        }
        _ => return None,
    })
}

fn operand(pair: Pair<'_, Rule>) -> Operand<'_> {
    let text = pair.as_str();
    match pair.as_rule() {
        Rule::register => Operand::Register(text),
        Rule::register_list => Operand::Registers(pair.into_inner().map(|p| p.as_str()).collect()),
        Rule::register_range => {
            let mut inner = pair.into_inner();
            let from = inner.next().map_or("", |p| p.as_str());
            Operand::RegisterRange(from, inner.next().map_or("", |p| p.as_str()))
        }
        Rule::method_ref => {
            let parts: Vec<_> = pair.into_inner().map(|p| p.as_str()).collect();
            Operand::Method {
                class: parts[0],
                name: parts[1],
                descriptor: parts[2],
            }
        }
        Rule::field_ref => {
            let parts: Vec<_> = pair.into_inner().map(|p| p.as_str()).collect();
            Operand::Field {
                class: parts[0],
                name: parts[1],
                type_desc: parts[2],
            }
        }
        Rule::string => Operand::String(text),
        Rule::label => Operand::Label(text),
        Rule::type_desc => Operand::Type(text),
        Rule::literal => Operand::Literal(text),
        _ => Operand::Other(text),
    }
}
