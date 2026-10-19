//! `${ <jmespath> }` expressions inside YAML strings: scanning, compiling, and evaluating against a
//! request envelope. Standard JMESPath via the `jmespath` crate.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::LazyLock;
use std::thread::LocalKey;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jmespath::functions::{ArgumentType, CustomFunction, Signature};
use jmespath::{
    Context, ErrorReason, JmespathError, Rcvar, Runtime, RuntimeError, SearchResult, Variable,
    interpret,
};
use serde_json::Value;

/// A YAML string split into literal text and `${ … }` expression sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Expression(String),
}

/// Split `input` into text and expressions. `\${` is a literal `${`; the closing `}` is found by
/// brace depth outside string literals, so `${ {a: b} }` and `${ join('}', tags) }` work. An
/// unterminated `${` is an error.
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
    // The open JMESPath literal's quote (`'`, `"` or a backtick) and whether the last char was `\`.
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in source.char_indices() {
        if let Some(open) = quote {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                _ if character == open => quote = None,
                _ => {}
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
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

/// Standard JMESPath functions plus Decoy's built-ins: `uuid()`, `now()`, `now('unix')`,
/// `random_int(min, max)`, `random_item(array)`.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    let mut runtime = Runtime::new();
    runtime.register_builtin_functions();
    let function =
        |inputs, variadic, f| Box::new(CustomFunction::new(Signature::new(inputs, variadic), f));
    runtime.register_function(
        "uuid",
        function(vec![], None, Box::new(|_, _| Ok(string(uuid_v4())))),
    );
    runtime.register_function(
        "now",
        function(vec![], Some(ArgumentType::String), Box::new(now)),
    );
    runtime.register_function(
        "random_int",
        function(
            vec![ArgumentType::Number, ArgumentType::Number],
            None,
            Box::new(random_int),
        ),
    );
    runtime.register_function(
        "random_item",
        function(vec![ArgumentType::Array], None, Box::new(random_item)),
    );
    runtime
});

fn compile(source: &str) -> Result<jmespath::Expression<'static>, String> {
    RUNTIME
        .compile(source)
        .map_err(|error| format!("invalid JMESPath `{source}`: {error}"))
}

type Cache = RefCell<HashMap<String, Rc<jmespath::Expression<'static>>>>;

thread_local! {
    /// Compiled expressions by source. Sources come from config files, so the set stays small;
    /// per thread because a compiled expression isn't `Send`.
    static COMPILED: Cache = RefCell::new(HashMap::new());
    /// The same, keyed by the whole predicate string, so a predicate isn't re-segmented either.
    static PREDICATES: Cache = RefCell::new(HashMap::new());
}

/// `compile`, once per source and thread: requests reuse the parsed expression.
fn compiled(source: &str) -> Result<Rc<jmespath::Expression<'static>>, String> {
    cached(&COMPILED, source, || compile(source))
}

fn cached(
    cache: &'static LocalKey<Cache>,
    key: &str,
    compile: impl FnOnce() -> Result<jmespath::Expression<'static>, String>,
) -> Result<Rc<jmespath::Expression<'static>>, String> {
    if let Some(expression) = cache.with(|cache| cache.borrow().get(key).cloned()) {
        return Ok(expression);
    }
    let expression = Rc::new(compile()?);
    cache.with(|cache| {
        cache
            .borrow_mut()
            .insert(key.to_owned(), Rc::clone(&expression))
    });
    Ok(expression)
}

/// The request envelope as JMESPath sees it. Convert once per request and share the result:
/// `Expression::search` would convert the whole envelope again on every call.
pub fn to_rcvar(envelope: &Value) -> Rcvar {
    // Can't fail: every JSON value has a JMESPath counterpart (no NaN or non-string keys).
    Rcvar::new(Variable::try_from(envelope).unwrap_or(Variable::Null))
}

/// Replace one top-level field of a converted envelope; copies only the top-level map if shared.
pub fn set_field(envelope: &mut Rcvar, key: &str, value: &Value) {
    if let Variable::Object(fields) = Rc::make_mut(envelope) {
        fields.insert(key.to_owned(), to_rcvar(value));
    }
}

fn search(expression: &jmespath::Expression<'_>, envelope: &Rcvar) -> SearchResult {
    interpret(
        envelope,
        expression.as_ast(),
        &mut Context::new(expression.as_str(), &RUNTIME),
    )
}

/// Check one expression source (without `${ }`) at startup.
pub fn validate_expression(source: &str) -> Result<(), String> {
    compile(source).map(|_| ())
}

/// Evaluate one expression source against `envelope`.
pub fn evaluate(source: &str, envelope: &Rcvar) -> Result<Value, String> {
    let result = search(&*compiled(source)?, envelope)
        .map_err(|error| format!("`{source}` failed: {error}"))?;
    serde_json::to_value(&*result).map_err(|error| format!("`{source}` result: {error}"))
}

fn string(text: String) -> Rcvar {
    Rcvar::new(Variable::String(text))
}

fn invalid(ctx: &Context<'_>, message: &str) -> JmespathError {
    JmespathError::from_ctx(
        ctx,
        ErrorReason::Runtime(RuntimeError::InvalidType {
            expected: message.to_owned(),
            actual: String::new(),
            position: 0,
        }),
    )
}

/// Random RFC 4122 version 4 UUID, lowercase.
fn uuid_v4() -> String {
    let mut bytes: [u8; 16] = std::array::from_fn(|_| fastrand::u8(..));
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `now()` → RFC 3339 UTC with milliseconds; `now('unix')` → whole seconds since the epoch.
fn now(args: &[Rcvar], ctx: &mut Context<'_>) -> Result<Rcvar, JmespathError> {
    let now = SystemTime::now();
    match args.first().map(|format| format.as_string().cloned()) {
        None => Ok(string(humantime::format_rfc3339_millis(now).to_string())),
        Some(Some(format)) if format == "unix" && args.len() == 1 => {
            let seconds = now
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            Ok(Rcvar::new(Variable::Number(seconds.into())))
        }
        _ => Err(invalid(ctx, "now() or now('unix')")),
    }
}

/// `random_int(min, max)`, both inclusive integers.
fn random_int(args: &[Rcvar], ctx: &mut Context<'_>) -> Result<Rcvar, JmespathError> {
    let bound = |index: usize| {
        args[index]
            .as_number()
            .filter(|number| number.fract() == 0.0)
            .map(|number| number as i64)
    };
    match (bound(0), bound(1)) {
        (Some(min), Some(max)) if min <= max => Ok(Rcvar::new(Variable::Number(
            fastrand::i64(min..=max).into(),
        ))),
        _ => Err(invalid(
            ctx,
            "random_int(min, max) with integers min <= max",
        )),
    }
}

/// `random_item(array)`: one element, or null for an empty array.
fn random_item(args: &[Rcvar], _: &mut Context<'_>) -> Result<Rcvar, JmespathError> {
    let items = args[0].as_array().cloned().unwrap_or_default();
    Ok(fastrand::choice(items).unwrap_or_else(|| Rcvar::new(Variable::Null)))
}

/// Check a predicate at startup so a bad one never reaches a request.
pub fn validate_predicate(predicate: &str) -> Result<(), String> {
    compile(&predicate_source(predicate)?).map(|_| ())
}

/// Evaluate a validated predicate against `envelope`. JMESPath truthiness: `null`, `false`, `""`,
/// `[]`, and `{}` are false. An evaluation error (e.g. a function given the wrong type) is
/// returned, and logged so the miss it causes isn't silent; the caller treats it as false.
pub fn predicate_holds(predicate: &str, envelope: &Rcvar) -> Result<bool, String> {
    let result = cached(&PREDICATES, predicate, || {
        compile(&predicate_source(predicate)?)
    })
    .and_then(|expression| search(&expression, envelope).map_err(|error| error.to_string()));
    match result {
        Ok(result) => Ok(result.is_truthy()),
        Err(error) => {
            // Once per predicate: a fallback Case checked on every request mustn't flood stderr.
            static LOGGED: std::sync::Mutex<std::collections::BTreeSet<String>> =
                std::sync::Mutex::new(std::collections::BTreeSet::new());
            if LOGGED
                .lock()
                .is_ok_and(|mut logged| logged.insert(predicate.to_owned()))
            {
                crate::events::warn(format!(
                    "predicate `{predicate}` failed, treated as false (logged once): {error}"
                ));
            }
            Err(error)
        }
    }
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
    fn closing_brace_skips_string_literals() {
        assert_eq!(
            segments(r#"${ join('}', tags) }"#).unwrap(),
            vec![Segment::Expression("join('}', tags)".to_owned())]
        );
        assert_eq!(
            segments(r#"${ "a}b" }${ `"}"` }${ 'it\'s}' }"#).unwrap(),
            vec![
                Segment::Expression(r#""a}b""#.to_owned()),
                Segment::Expression(r#"`"}"`"#.to_owned()),
                Segment::Expression(r"'it\'s}'".to_owned()),
            ]
        );
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
    fn built_in_functions() {
        let envelope = to_rcvar(&json!({ "pick": ["a", "b"] }));
        let uuid = evaluate("uuid()", &envelope).unwrap();
        let uuid = uuid.as_str().unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(evaluate("uuid()", &envelope), evaluate("uuid()", &envelope));

        let now = evaluate("now()", &envelope).unwrap();
        assert!(
            humantime::parse_rfc3339(now.as_str().unwrap()).is_ok(),
            "{now}"
        );
        let unix = evaluate("now('unix')", &envelope)
            .unwrap()
            .as_u64()
            .unwrap();
        assert!(unix > 1_700_000_000);
        assert!(evaluate("now('iso')", &envelope).is_err());

        for _ in 0..50 {
            let value = evaluate("random_int(`1`, `3`)", &envelope)
                .unwrap()
                .as_i64()
                .unwrap();
            assert!((1..=3).contains(&value));
            let item = evaluate("random_item(pick)", &envelope).unwrap();
            assert!(item == json!("a") || item == json!("b"));
        }
        assert!(evaluate("random_int(`3`, `1`)", &envelope).is_err());
        assert_eq!(
            evaluate("random_item(`[]`)", &envelope).unwrap(),
            Value::Null
        );
        // Standard functions still work next to Decoy's.
        assert_eq!(evaluate("length(pick)", &envelope).unwrap(), json!(2));
    }

    #[test]
    fn predicates_follow_jmespath_truthiness() {
        let envelope = to_rcvar(&json!({
            "body": { "amount": 1500, "operationName": "GetUser", "empty": "", "items": [] },
            "headers": { "x-tier": "gold" }
        }));
        assert_eq!(
            predicate_holds("${ body.amount > `1000` }", &envelope),
            Ok(true)
        );
        assert_eq!(
            predicate_holds(
                "${ body.operationName == 'GetUser' && headers.\"x-tier\" == 'gold' }",
                &envelope
            ),
            Ok(true)
        );
        assert_eq!(predicate_holds("${ body.missing }", &envelope), Ok(false));
        assert_eq!(predicate_holds("${ body.empty }", &envelope), Ok(false));
        assert_eq!(predicate_holds("${ body.items }", &envelope), Ok(false));
        assert_eq!(predicate_holds("${ body.amount }", &envelope), Ok(true));
        // A runtime error is returned, not just false, so a miss can show it.
        assert!(predicate_holds("${ abs(headers.\"x-tier\") }", &envelope).is_err());
    }
}
