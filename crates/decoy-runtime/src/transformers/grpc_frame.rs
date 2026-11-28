use super::TransformError;

/// Remove one uncompressed length-prefixed frame.
pub(crate) fn input(bytes: &[u8]) -> Result<&[u8], TransformError> {
    let [compressed, a, b, c, d, rest @ ..] = bytes else {
        return Err(TransformError::ShortFrame);
    };
    if *compressed != 0 {
        return Err(TransformError::Compressed);
    }
    let length = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if rest.len() != length {
        return Err(TransformError::FrameLength {
            declared: length,
            carried: rest.len(),
        });
    }
    Ok(rest)
}

/// Add one uncompressed length-prefixed frame.
pub(crate) fn output(message: &[u8]) -> Vec<u8> {
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(message.len() + 5);
    framed.push(0);
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    framed
}
