use std::collections::BTreeMap;

/// The param names of a path template, or why a segment is neither literal text nor a whole
/// `{param}` (params only bind whole segments, so `/files/{name}.json` could never match).
pub(crate) fn template_params(pattern: &str) -> Result<Vec<&str>, String> {
    let mut params = Vec::new();
    for part in split_path(pattern) {
        let name = part
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'));
        match name {
            Some(name) if !name.is_empty() && !name.contains(['{', '}']) => params.push(name),
            None if !part.contains(['{', '}']) => {}
            _ => {
                return Err(format!(
                    "segment `{part}` must be literal text or a whole `{{param}}`"
                ));
            }
        }
    }
    Ok(params)
}

pub(crate) fn match_path(pattern: &str, actual: &str) -> Option<BTreeMap<String, String>> {
    let mut actual_parts = split_path(actual);
    let mut params = BTreeMap::new();
    for pattern_part in split_path(pattern) {
        let actual_part = actual_parts.next()?;
        if pattern_part.starts_with('{') && pattern_part.ends_with('}') {
            let name = &pattern_part[1..pattern_part.len() - 1];
            // Decoded like query values, so `/users/a%20b` gives `a b`.
            let value = percent_encoding::percent_decode_str(actual_part).decode_utf8_lossy();
            params.insert(name.to_owned(), value.into_owned());
            continue;
        }

        if pattern_part != percent_encoding::percent_decode_str(actual_part).decode_utf8_lossy() {
            return None;
        }
    }

    actual_parts.next().is_none().then_some(params)
}

pub(crate) fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|part| !part.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_bind_one_nonempty_segment_before_percent_decoding() {
        let params = match_path("/users/{id}", "//users//a%2Fb/").unwrap();
        assert_eq!(params["id"], "a/b");
        assert!(match_path("/users/{id}", "/users/").is_none());
        assert!(match_path("/users/{id}", "/users/a/b").is_none());
    }
}
