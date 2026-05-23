//! Handlebars-lite template renderer + system-prompt composer.
//!
//! Supports:
//!   - `{{var}}`            — string substitution; missing renders as empty.
//!   - `{{#if var}}…{{/if}}` (with optional `{{else}}`) — block; truthy iff
//!     value is present and non-empty/non-zero.
//!
//! Deliberately tiny — we never want a full handlebars dep for a 3-feature
//! template language.

use crate::skills::skill::Skill;
use serde_json::Value;
use std::collections::BTreeMap;

/// Render a template body against a JSON-like context map.
pub fn render(template: &str, ctx: &BTreeMap<String, Value>) -> String {
    let tokens = tokenize(template);
    render_tokens(&tokens, ctx)
}

#[derive(Debug)]
enum Token {
    Text(String),
    Var(String),
    IfOpen(String),
    Else,
    EndIf,
}

fn tokenize(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut text_start = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if i > text_start {
                out.push(Token::Text(src[text_start..i].to_string()));
            }
            // find closing }}
            let close = src[i + 2..].find("}}");
            let end = match close {
                Some(e) => i + 2 + e,
                None => break,
            };
            let inner = src[i + 2..end].trim();
            if let Some(rest) = inner.strip_prefix("#if ") {
                out.push(Token::IfOpen(rest.trim().to_string()));
            } else if inner == "else" {
                out.push(Token::Else);
            } else if inner == "/if" {
                out.push(Token::EndIf);
            } else {
                out.push(Token::Var(inner.to_string()));
            }
            i = end + 2;
            text_start = i;
        } else {
            i += 1;
        }
    }
    if text_start < bytes.len() {
        out.push(Token::Text(src[text_start..].to_string()));
    }
    out
}

fn render_tokens(tokens: &[Token], ctx: &BTreeMap<String, Value>) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Text(t) => out.push_str(t),
            Token::Var(name) => out.push_str(&value_to_string(ctx.get(name))),
            Token::IfOpen(name) => {
                let (then_part, else_part, after) = split_if(tokens, i);
                if truthy(ctx.get(name)) {
                    out.push_str(&render_tokens(then_part, ctx));
                } else if let Some(else_part) = else_part {
                    out.push_str(&render_tokens(else_part, ctx));
                }
                i = after;
                continue;
            }
            // EndIf / Else are consumed by the IfOpen branch.
            Token::Else | Token::EndIf => {}
        }
        i += 1;
    }
    out
}

/// Locate matching `{{else}}` and `{{/if}}` for the IfOpen at `start`.
/// Returns `(then_tokens, else_tokens, index_after_endif)`.
fn split_if(tokens: &[Token], start: usize) -> (&[Token], Option<&[Token]>, usize) {
    let mut depth = 1;
    let mut else_at: Option<usize> = None;
    let mut end_at = start + 1;
    for (idx, t) in tokens.iter().enumerate().skip(start + 1) {
        match t {
            Token::IfOpen(_) => depth += 1,
            Token::EndIf => {
                depth -= 1;
                if depth == 0 {
                    end_at = idx;
                    break;
                }
            }
            Token::Else if depth == 1 => {
                else_at = Some(idx);
            }
            _ => {}
        }
    }
    let after = end_at + 1;
    let then_part = match else_at {
        Some(e) => &tokens[start + 1..e],
        None => &tokens[start + 1..end_at],
    };
    let else_part = else_at.map(|e| &tokens[e + 1..end_at]);
    (then_part, else_part, after)
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::Array(arr)) => !arr.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Result of composing the system prompt with a set of skills.
pub struct ComposeResult {
    /// The full prompt: base + skills + caller-supplied suffix (if any).
    pub prompt: String,
    /// Number of characters of skill content actually included.
    pub composed_chars: usize,
    /// Skill ids that were dropped because they exceeded the budget.
    pub dropped_for_budget: Vec<String>,
}

/// Compose the final system prompt from a base, an ordered list of selected
/// skills, a per-skill context map, and a soft character budget.
pub fn compose_system_prompt(
    base: &str,
    selected: &[&Skill],
    ctx: &BTreeMap<String, Value>,
    char_budget: usize,
) -> ComposeResult {
    let mut out = String::with_capacity(base.len() + 4096);
    out.push_str(base);
    let mut spent = 0usize;
    let mut dropped: Vec<String> = Vec::new();
    if !selected.is_empty() {
        out.push_str("\n\n[ACTIVE SKILLS]\n");
        for sk in selected {
            let body = render(&sk.body, ctx);
            let block = format!(
                "\n[SKILL: {} (id={}, src={})]\n{}\n[/SKILL]\n",
                sk.frontmatter.name,
                sk.id,
                sk.source.as_str(),
                body.trim_end()
            );
            if spent + block.len() > char_budget {
                dropped.push(sk.id.clone());
                continue;
            }
            spent += block.len();
            out.push_str(&block);
        }
    }
    ComposeResult {
        prompt: out,
        composed_chars: spent,
        dropped_for_budget: dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::skill::{parse, SkillSourceTag};
    use serde_json::json;

    fn ctx_of(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn substitutes_vars() {
        let ctx = ctx_of(&[("name", json!("World"))]);
        assert_eq!(render("Hello {{name}}!", &ctx), "Hello World!");
    }

    #[test]
    fn missing_var_renders_empty() {
        let ctx = ctx_of(&[]);
        assert_eq!(render("X{{missing}}Y", &ctx), "XY");
    }

    #[test]
    fn if_block_truthy() {
        let ctx = ctx_of(&[("v", json!("present"))]);
        assert_eq!(render("a{{#if v}}b{{/if}}c", &ctx), "abc");
    }

    #[test]
    fn if_block_falsy_uses_else() {
        let ctx = ctx_of(&[]);
        assert_eq!(render("a{{#if v}}b{{else}}B{{/if}}c", &ctx), "aBc");
    }

    #[test]
    fn nested_if() {
        let ctx = ctx_of(&[("a", json!("x")), ("b", json!("y"))]);
        let tpl = "[{{#if a}}A[{{#if b}}B{{/if}}]{{/if}}]";
        assert_eq!(render(tpl, &ctx), "[A[B]]");
    }

    #[test]
    fn compose_includes_skill_blocks() {
        let raw = "---\nid: xx\nname: X\ndescription: d\n---\nbody {{n}}\n";
        let sk = parse("/p", SkillSourceTag::Builtin, raw).unwrap().unwrap();
        let ctx = ctx_of(&[("n", json!("hi"))]);
        let r = compose_system_prompt("BASE", &[&sk], &ctx, 4096);
        assert!(r.prompt.contains("[ACTIVE SKILLS]"));
        assert!(r.prompt.contains("body hi"));
        assert!(r.prompt.contains("[SKILL: X (id=xx, src=built-in)]"));
    }

    #[test]
    fn compose_respects_char_budget() {
        let raw = "---\nid: xx\nname: X\ndescription: d\n---\n";
        let body = "x".repeat(2000);
        let raw = format!("{}{}\n", raw, body);
        let sk = parse("/p", SkillSourceTag::Builtin, &raw).unwrap().unwrap();
        let r = compose_system_prompt("BASE", &[&sk], &BTreeMap::new(), 100);
        assert_eq!(r.dropped_for_budget, vec!["xx".to_string()]);
    }
}
