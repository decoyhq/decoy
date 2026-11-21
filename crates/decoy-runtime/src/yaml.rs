//! Decode authored YAML directly from source so schema errors retain their locations.
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Plain(#[from] serde_norway::Error),
    #[error(transparent)]
    Merge(Box<serde_saphyr::Error>),
}

impl Error {
    pub(crate) fn line(&self) -> Option<usize> {
        match self {
            Self::Plain(error) => error.location().map(|loc| loc.line()),
            Self::Merge(error) => error
                .locations()
                .map(|loc| loc.defined_location.line() as usize),
        }
    }
}

fn has_merge_key(input: &str) -> bool {
    use serde_saphyr::granit_parser::{ScalarStyle, Scanner, StrInput, TokenType};

    if !input.contains("<<") {
        return false;
    }
    let mut key = false;
    for token in Scanner::new(StrInput::new(input)).map_while(Result::ok) {
        match token.token_type() {
            TokenType::Key => key = true,
            TokenType::Anchor(_) | TokenType::Tag(_, _) => {}
            TokenType::Scalar(ScalarStyle::Plain, text) if key && text == "<<" => return true,
            _ => key = false,
        }
    }
    false
}

pub(crate) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, Error> {
    // Keep the existing decoder and diagnostics for ordinary files. Decoding a merged
    // serde_norway::Value would discard the source positions of inherited fields.
    if !has_merge_key(input) {
        return Ok(serde_norway::from_str(input)?);
    }
    let options = serde_saphyr::options! {
        strict_booleans: true,
        leading_zero_numbers_as_strings: true,
        underscored_numbers_as_strings: true,
        null_as_string: true,
        with_snippet: false,
    };
    serde_saphyr::from_str_with_options(input, options)
        .map_err(|error| Error::Merge(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_merges_preserve_quoted_keys_and_reject_non_mappings() {
        let input = r#"
base: &base { '<<': literal, keep: 01 }
middle: &middle { <<: *base, extra: yes }
last: { <<: *middle, extra: changed }
"#;
        let parsed: serde_json::Value = from_str(input).unwrap();
        assert_eq!(parsed["middle"]["extra"], "yes");
        assert_eq!(
            parsed["last"],
            serde_json::json!({"<<": "literal", "keep": "01", "extra": "changed"})
        );
        assert!(from_str::<serde_json::Value>("bad: { <<: [42] }").is_err());
    }

    #[test]
    fn literal_merge_text_keeps_its_meaning() {
        let input = "# <<: *anchor\nbody: { '<<': literal, value: << }";
        assert!(!has_merge_key(input));
        let parsed: serde_json::Value = from_str(input).unwrap();
        assert_eq!(parsed["body"]["<<"], "literal");
        assert_eq!(parsed["body"]["value"], "<<");
    }
}
