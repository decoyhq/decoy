//! `${ <jmespath> }` expressions inside YAML strings: scanning, compiling, and evaluating against a
//! request context. Standard JMESPath via the `jmespath` crate.

use serde_json::Value;

/// A YAML string split into literal text and `${ … }` expression sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Expression(String),
}

/// Split `input` into text and expressions. `\${` is a literal `${`; the closing `}` is found by
/// brace depth, so `${ {a: b} }` works. An unterminated `${` is an error.
pub fn segments(input: &str) -> Result<Vec<Segment>, String> {
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut rest = input;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("\\${") {
            text.push_str("${");
            rest = after;
        } else if let Some(after) = rest.strip_prefix("${") {
            let end =
                closing_brace(after).ok_or_else(|| format!("unterminated `${{` in {input:?}"))?;
            if !text.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut text)));
            }
            segments.push(Segment::Expression(after[..end].trim().to_owned()));
            rest = &after[end + 1..];
        } else {
            let next = rest.chars().next().unwrap_or_default();
            text.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    if !text.is_empty() {
        segments.push(Segment::Text(text));
    }
    Ok(segments)
}

fn closing_brace(source: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in source.char_indices() {
        match character {
            '{' => depth += 1,
            '}' if depth == 0 => return Some(index),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The single expression of a predicate string, which must be exactly `${ … }`.
fn predicate_source(predicate: &str) -> Result<String, String> {
    match segments(predicate)?.as_slice() {
        [Segment::Expression(source)] => Ok(source.clone()),
        _ => Err(format!(
            "predicate {predicate:?} must be exactly one `${{ <jmespath> }}` expression"
        )),
    }
}

fn compile(source: &str) -> Result<jmespath::Expression<'static>, String> {
    jmespath::compile(source).map_err(|error| format!("invalid JMESPath `{source}`: {error}"))
}

/// Check a predicate at startup so a bad one never reaches a request.
pub fn validate_predicate(predicate: &str) -> Result<(), String> {
    compile(&predicate_source(predicate)?).map(|_| ())
}

/// Evaluate a validated predicate against `context`. JMESPath truthiness: `null`, `false`, `""`,
/// `[]`, and `{}` are false. An evaluation error (e.g. a function given the wrong type) is false.
pub fn predicate_holds(predicate: &str, context: &Value) -> bool {
    let Ok(expression) = predicate_source(predicate).and_then(|source| compile(&source)) else {
        return false;
    };
    expression
        .search(context)
        .is_ok_and(|result| result.is_truthy())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn splits_text_and_expressions() {
        assert_eq!(
            segments("Hi ${ body.name }!").unwrap(),
            vec![
                Segment::Text("Hi ".to_owned()),
                Segment::Expression("body.name".to_owned()),
                Segment::Text("!".to_owned()),
            ]
        );
        assert_eq!(
            segments("${a}${b}").unwrap(),
            vec![
                Segment::Expression("a".to_owned()),
                Segment::Expression("b".to_owned()),
            ]
        );
        assert_eq!(segments("").unwrap(), vec![]);
    }

    #[test]
    fn escaped_dollar_brace_is_literal() {
        assert_eq!(
            segments(r"cost \${price} = ${ body.price }").unwrap(),
            vec![
                Segment::Text("cost ${price} = ".to_owned()),
                Segment::Expression("body.price".to_owned()),
            ]
        );
    }

    #[test]
    fn closing_brace_respects_nesting() {
        assert_eq!(
            segments("${ {id: body.id} }").unwrap(),
            vec![Segment::Expression("{id: body.id}".to_owned())]
        );
        assert!(segments("${ body.id").is_err());
    }

    #[test]
    fn predicates_must_be_one_valid_expression() {
        assert!(validate_predicate("${ body.amount > `1000` }").is_ok());
        assert!(validate_predicate("body.amount").is_err());
        assert!(validate_predicate("x ${ body.amount }").is_err());
        assert!(
            validate_predicate("${ body.[ }")
                .unwrap_err()
                .contains("invalid JMESPath")
        );
    }

    #[test]
    fn predicates_follow_jmespath_truthiness() {
        let context = json!({
            "body": { "amount": 1500, "operationName": "GetUser", "empty": "", "items": [] },
            "headers": { "x-tier": "gold" }
        });
        assert!(predicate_holds("${ body.amount > `1000` }", &context));
        assert!(predicate_holds(
            "${ body.operationName == 'GetUser' && headers.\"x-tier\" == 'gold' }",
            &context
        ));
        assert!(!predicate_holds("${ body.missing }", &context));
        assert!(!predicate_holds("${ body.empty }", &context));
        assert!(!predicate_holds("${ body.items }", &context));
        assert!(predicate_holds("${ body.amount }", &context));
    }
}
