//! Framing: a 4-byte big-endian length, then one msgpack map with a string `kind`.

use rmpv::Value as Msgpack;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Longest message either side may send.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// Builds a message map from `kind` and its fields.
pub fn message(kind: &str, fields: impl IntoIterator<Item = (&'static str, Msgpack)>) -> Msgpack {
    Msgpack::Map(
        std::iter::once(("kind", Msgpack::from(kind)))
            .chain(fields)
            .map(|(key, value)| (Msgpack::from(key), value))
            .collect(),
    )
}

/// `message[field]`, or nil.
pub fn field<'a>(message: &'a Msgpack, field: &str) -> &'a Msgpack {
    static NIL: Msgpack = Msgpack::Nil;
    message
        .as_map()
        .and_then(|entries| entries.iter().find(|(key, _)| key.as_str() == Some(field)))
        .map_or(&NIL, |(_, value)| value)
}

pub async fn write(out: &mut (impl AsyncWrite + Unpin), message: &Msgpack) -> std::io::Result<()> {
    let mut body = Vec::new();
    rmpv::encode::write_value(&mut body, message).expect("writing to a Vec doesn't fail");
    let length = u32::try_from(body.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    out.write_all(&frame).await?;
    out.flush().await
}

/// How many bytes `message` takes on the wire, without its length prefix.
pub fn encoded_len(message: &Msgpack) -> usize {
    let mut body = Vec::new();
    rmpv::encode::write_value(&mut body, message).expect("writing to a Vec doesn't fail");
    body.len()
}

/// The next message, `Ok(None)` at a clean end of stream.
pub async fn read(input: &mut (impl AsyncRead + Unpin)) -> Result<Option<Msgpack>, String> {
    let mut head = [0; 4];
    match input.read_exact(&mut head).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let length = u32::from_be_bytes(head) as usize;
    if head
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
    {
        // No message is ~500 MB; this is text, e.g. a `console.log` before `definePlugin`.
        return Err(format!(
            "it wrote text to stdout (`{}…`), which carries the protocol; plugins log to stderr",
            String::from_utf8_lossy(&head)
        ));
    }
    if length > MAX_MESSAGE {
        return Err(format!(
            "message of {length} bytes is over the {MAX_MESSAGE} byte limit"
        ));
    }
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .await
        .map_err(|error| format!("stdout ended inside a message: {error}"))?;
    let message = rmpv::decode::read_value(&mut body.as_slice())
        .map_err(|error| format!("message is not msgpack: {error}"))?;
    if field(&message, "kind").as_str().is_none() {
        return Err("message is not a map with a string `kind`".to_owned());
    }
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_round_trip_and_bad_frames_are_errors() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let sent = message("hello", [("protocol", 1.into())]);
        write(&mut a, &sent).await.unwrap();
        assert_eq!(read(&mut b).await.unwrap(), Some(sent));

        a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        assert!(read(&mut b).await.unwrap_err().contains("byte limit"));
        a.write_all(b"hello from plugin\n").await.unwrap();
        assert_eq!(
            read(&mut b).await.unwrap_err(),
            "it wrote text to stdout (`hell…`), which carries the protocol; plugins log to stderr"
        );

        let (mut a, mut b) = tokio::io::duplex(1024);
        write(&mut a, &Msgpack::from(7)).await.unwrap();
        assert!(read(&mut b).await.unwrap_err().contains("`kind`"));
        drop(a);
        assert_eq!(read(&mut b).await.unwrap(), None);
    }
}
