//! Decode authored YAML directly from source so schema errors retain their locations.
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Plain(#[from] serde_norway::Error),
    #[error(transparent)]
    Merge(Box<serde_saphyr::Error>),
}

impl Error {
    pub(crate) fn line(&self) -> Option<usize> {
        match self {
            Self::Plain(error) => error.location().map(|loc| loc.line()),
            Self::Merge(error) => merged_error_line(error),
        }
    }
}

pub(crate) fn merged_error_line(error: &serde_saphyr::Error) -> Option<usize> {
    error
        .locations()
        .map(|loc| loc.defined_location.line() as usize)
}

fn needs_merge_decoder(input: &str) -> bool {
    use serde_saphyr::granit_parser::{ScalarStyle, Scanner, StrInput, TokenType};

    if !input.contains("<<") && !input.contains('!') {
        return false;
    }
    let mut key = false;
    let mut tagged = false;
    for token in Scanner::new(StrInput::new(input)).map_while(Result::ok) {
        match token.token_type() {
            TokenType::Key => {
                key = true;
                tagged = false;
            }
            TokenType::Anchor(_) => {}
            TokenType::Tag(_, _) => tagged = true,
            // Let the decoder resolve explicit tags, including custom %TAG handles.
            TokenType::Scalar(style, text)
                if key && text == "<<" && (tagged || *style == ScalarStyle::Plain) =>
            {
                return true;
            }
            _ => key = false,
        }
    }
    false
}

pub(crate) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, Error> {
    // Keep the existing decoder and diagnostics for ordinary files. Decoding a merged
    // serde_norway::Value would discard the source positions of inherited fields.
    if !needs_merge_decoder(input) {
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
    fn explicitly_tagged_quoted_merge_keys_expand() {
        for (directive, tag) in [
            ("", "!!merge"),
            ("", "!<tag:yaml.org,2002:merge>"),
            ("%TAG !y! tag:yaml.org,2002:\n---\n", "!y!merge"),
        ] {
            let input =
                format!("{directive}base: &base {{ port: 9000 }}\ncopy: {{ {tag} '<<': *base }}");
            let parsed: serde_json::Value = from_str(&input).unwrap();
            assert_eq!(parsed["copy"], parsed["base"], "{tag}");
        }
        let parsed: serde_json::Value = from_str("!!str '<<': literal").unwrap();
        assert_eq!(parsed["<<"], "literal");
    }

    #[test]
    fn literal_merge_text_keeps_its_meaning() {
        let input = "# <<: *anchor\nbody: { '<<': literal, value: << }";
        assert!(!needs_merge_decoder(input));
        let parsed: serde_json::Value = from_str(input).unwrap();
        assert_eq!(parsed["body"]["<<"], "literal");
        assert_eq!(parsed["body"]["value"], "<<");
    }
}
